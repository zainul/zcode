//! Unified-diff parsing and application (native "apply diff patches" tool).
//!
//! Applying a patch in-process — rather than shelling out to `patch(1)` — means
//! it works identically on every platform, is not subject to the shell
//! allowlist, and can report a precise, model-readable reason when a hunk does
//! not fit.
//!
//! Line numbers in model-produced diffs drift, so hunks are located by matching
//! their context rather than by trusting `@@` offsets; the stated position is
//! only a starting hint.

use std::fmt;
use std::path::{Path, PathBuf};

/// How far from the hinted position a hunk may be found.
const SEARCH_RADIUS: usize = 200;

#[derive(Debug, PartialEq, Eq)]
pub enum PatchError {
    /// Nothing that looks like a unified diff.
    Empty,
    Malformed(String),
    /// A hunk's context could not be located in the target file.
    ContextNotFound {
        path: String,
        hunk: usize,
    },
    /// The patch targets a file that does not exist (and is not a creation).
    MissingFile(String),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(
                f,
                "no unified-diff hunks found — expected `--- a/file`, `+++ b/file`, `@@ ... @@`"
            ),
            Self::Malformed(m) => write!(f, "malformed patch: {m}"),
            Self::ContextNotFound { path, hunk } => write!(
                f,
                "hunk {hunk} does not match {path} — re-read the file and rebuild the diff \
                 from its current contents"
            ),
            Self::MissingFile(p) => write!(f, "patch targets missing file: {p}"),
        }
    }
}

impl std::error::Error for PatchError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Removed(String),
    Added(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based line hint from the `@@` header; treated as advisory.
    pub old_start: usize,
    pub lines: Vec<HunkLine>,
}

impl Hunk {
    /// The lines this hunk expects to find (context + removed).
    fn expected(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(t) | HunkLine::Removed(t) => Some(t.as_str()),
                HunkLine::Added(_) => None,
            })
            .collect()
    }

    /// The lines this hunk leaves behind (context + added).
    fn replacement(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(t) | HunkLine::Added(t) => Some(t.as_str()),
                HunkLine::Removed(_) => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchAction {
    Modify,
    Create,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub path: String,
    pub action: PatchAction,
    pub hunks: Vec<Hunk>,
}

/// Strip the `a/` or `b/` prefix git puts on diff paths.
fn clean_path(raw: &str) -> String {
    let raw = raw.split('\t').next().unwrap_or(raw).trim();
    raw.strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw)
        .to_string()
}

fn parse_hunk_header(line: &str) -> Option<usize> {
    // `@@ -12,7 +12,9 @@ optional trailing context`
    let rest = line.strip_prefix("@@")?;
    let old = rest.split_whitespace().find(|t| t.starts_with('-'))?;
    let digits: String = old
        .trim_start_matches('-')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<usize>().ok()
}

/// Parse a unified diff covering one or more files.
pub fn parse_unified_diff(text: &str) -> Result<Vec<FilePatch>, PatchError> {
    let mut patches: Vec<FilePatch> = Vec::new();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        if !line.starts_with("--- ") {
            continue;
        }
        let old_path = clean_path(&line[4..]);
        let Some(next) = lines.next() else {
            return Err(PatchError::Malformed("`---` without `+++`".into()));
        };
        if !next.starts_with("+++ ") {
            return Err(PatchError::Malformed(format!(
                "expected `+++` after `---`, got `{next}`"
            )));
        }
        let new_path = clean_path(&next[4..]);

        let action = if old_path == "/dev/null" {
            PatchAction::Create
        } else if new_path == "/dev/null" {
            PatchAction::Delete
        } else {
            PatchAction::Modify
        };
        let path = if action == PatchAction::Create {
            new_path
        } else {
            old_path
        };

        let mut hunks: Vec<Hunk> = Vec::new();
        while let Some(peeked) = lines.peek() {
            if peeked.starts_with("--- ") || peeked.starts_with("diff ") {
                break;
            }
            let current = lines.next().expect("peeked");
            if let Some(old_start) = parse_hunk_header(current) {
                hunks.push(Hunk {
                    old_start,
                    lines: Vec::new(),
                });
                continue;
            }
            let Some(hunk) = hunks.last_mut() else {
                continue; // preamble noise between the header and the first @@
            };
            // "\ No newline at end of file" is metadata, not content.
            if current.starts_with('\\') {
                continue;
            }
            match current.chars().next() {
                Some('+') => hunk.lines.push(HunkLine::Added(current[1..].to_string())),
                Some('-') => hunk.lines.push(HunkLine::Removed(current[1..].to_string())),
                Some(' ') => hunk.lines.push(HunkLine::Context(current[1..].to_string())),
                // A completely empty line inside a hunk is an empty context line.
                None => hunk.lines.push(HunkLine::Context(String::new())),
                Some(_) => break, // end of this file's hunks
            }
        }

        if hunks.is_empty() && action != PatchAction::Delete {
            return Err(PatchError::Malformed(format!("no hunks for {path}")));
        }
        patches.push(FilePatch {
            path,
            action,
            hunks,
        });
    }

    if patches.is_empty() {
        return Err(PatchError::Empty);
    }
    Ok(patches)
}

/// Locate `expected` in `lines`, starting from `hint` and widening outwards.
/// Returns the index where the match begins.
fn find_match(lines: &[String], expected: &[&str], hint: usize) -> Option<usize> {
    if expected.is_empty() {
        return Some(hint.min(lines.len()));
    }
    if expected.len() > lines.len() {
        return None;
    }
    let last_start = lines.len() - expected.len();
    let hint = hint.min(last_start);

    let matches_at = |start: usize| {
        lines[start..start + expected.len()]
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    };
    if matches_at(hint) {
        return Some(hint);
    }
    // Search outwards so the candidate closest to the stated position wins.
    for delta in 1..=SEARCH_RADIUS.min(lines.len()) {
        if let Some(before) = hint.checked_sub(delta) {
            if matches_at(before) {
                return Some(before);
            }
        }
        let after = hint + delta;
        if after <= last_start && matches_at(after) {
            return Some(after);
        }
    }
    None
}

/// Apply every hunk to `original`, returning the patched text.
pub fn apply_hunks(original: &str, hunks: &[Hunk], path: &str) -> Result<String, PatchError> {
    let had_trailing_newline = original.ends_with('\n') || original.is_empty();
    let mut lines: Vec<String> = original.lines().map(String::from).collect();

    for (index, hunk) in hunks.iter().enumerate() {
        let expected = hunk.expected();
        let hint = hunk.old_start.saturating_sub(1);
        let Some(at) = find_match(&lines, &expected, hint) else {
            return Err(PatchError::ContextNotFound {
                path: path.to_string(),
                hunk: index + 1,
            });
        };
        let replacement: Vec<String> = hunk.replacement().into_iter().map(String::from).collect();
        lines.splice(at..at + expected.len(), replacement);
    }

    let mut out = lines.join("\n");
    if had_trailing_newline && !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

/// One file's outcome, for the summary handed back to the model.
#[derive(Debug, PartialEq, Eq)]
pub struct AppliedFile {
    pub path: PathBuf,
    pub action: PatchAction,
    pub hunks: usize,
}

/// Apply a whole unified diff beneath `root`.
///
/// The patch is validated and applied file by file; the first failure aborts
/// with a descriptive error rather than leaving a half-applied patch behind on
/// files it has not reached yet.
pub fn apply_patch(root: &Path, patch_text: &str) -> Result<Vec<AppliedFile>, PatchError> {
    let patches = parse_unified_diff(patch_text)?;
    let mut planned: Vec<(PathBuf, PatchAction, usize, Option<String>)> =
        Vec::with_capacity(patches.len());

    // Pass 1: compute every result before writing anything, so a patch that
    // fails on its third file does not leave the first two rewritten.
    for file in &patches {
        let path = resolve_under(root, &file.path);
        match file.action {
            PatchAction::Delete => {
                planned.push((path, PatchAction::Delete, file.hunks.len(), None))
            }
            PatchAction::Create => {
                let created = apply_hunks("", &file.hunks, &file.path)?;
                planned.push((path, PatchAction::Create, file.hunks.len(), Some(created)));
            }
            PatchAction::Modify => {
                let current = std::fs::read_to_string(&path)
                    .map_err(|_| PatchError::MissingFile(file.path.clone()))?;
                let patched = apply_hunks(&current, &file.hunks, &file.path)?;
                planned.push((path, PatchAction::Modify, file.hunks.len(), Some(patched)));
            }
        }
    }

    // Pass 2: write.
    let mut applied = Vec::with_capacity(planned.len());
    for (path, action, hunks, content) in planned {
        match (&action, content) {
            (PatchAction::Delete, _) => {
                std::fs::remove_file(&path).map_err(|e| {
                    PatchError::Malformed(format!("cannot delete {}: {e}", path.display()))
                })?;
            }
            (_, Some(text)) => {
                write_atomic(&path, &text).map_err(|e| {
                    PatchError::Malformed(format!("cannot write {}: {e}", path.display()))
                })?;
            }
            (_, None) => {}
        }
        applied.push(AppliedFile {
            path,
            action,
            hunks,
        });
    }
    Ok(applied)
}

fn resolve_under(root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        root.join(p)
    }
}

fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("zcode-patch-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "--- a/src/lib.rs\n\
                          +++ b/src/lib.rs\n\
                          @@ -1,3 +1,3 @@\n\
                          \x20fn main() {\n\
                          -    println!(\"old\");\n\
                          +    println!(\"new\");\n\
                          \x20}\n";

    #[test]
    fn parses_a_single_file_patch() {
        let patches = parse_unified_diff(SIMPLE).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "src/lib.rs");
        assert_eq!(patches[0].action, PatchAction::Modify);
        assert_eq!(patches[0].hunks.len(), 1);
        assert_eq!(patches[0].hunks[0].old_start, 1);
        assert_eq!(patches[0].hunks[0].lines.len(), 4);
    }

    #[test]
    fn applies_a_replacement() {
        let original = "fn main() {\n    println!(\"old\");\n}\n";
        let patches = parse_unified_diff(SIMPLE).unwrap();
        let out = apply_hunks(original, &patches[0].hunks, "src/lib.rs").unwrap();
        assert_eq!(out, "fn main() {\n    println!(\"new\");\n}\n");
    }

    #[test]
    fn tolerates_drifted_line_numbers() {
        // The model claims the hunk is at line 1; it is really at line 4.
        let original = "// header\n// header\n// header\nfn main() {\n    println!(\"old\");\n}\n";
        let patches = parse_unified_diff(SIMPLE).unwrap();
        let out = apply_hunks(original, &patches[0].hunks, "src/lib.rs").unwrap();
        assert!(out.contains("println!(\"new\")"));
        assert!(out.starts_with("// header"));
    }

    #[test]
    fn reports_a_hunk_that_does_not_fit() {
        let original = "something entirely different\n";
        let patches = parse_unified_diff(SIMPLE).unwrap();
        let err = apply_hunks(original, &patches[0].hunks, "src/lib.rs").unwrap_err();
        assert_eq!(
            err,
            PatchError::ContextNotFound {
                path: "src/lib.rs".into(),
                hunk: 1
            }
        );
        // The message must tell the model how to recover.
        assert!(err.to_string().contains("re-read the file"));
    }

    #[test]
    fn applies_multiple_hunks_in_one_file() {
        let original = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
        let patch = "--- a/f.txt\n+++ b/f.txt\n\
                     @@ -1,2 +1,2 @@\n one\n-two\n+TWO\n\
                     @@ -6,2 +6,2 @@\n six\n-seven\n+SEVEN\n";
        let patches = parse_unified_diff(patch).unwrap();
        assert_eq!(patches[0].hunks.len(), 2);
        let out = apply_hunks(original, &patches[0].hunks, "f.txt").unwrap();
        assert_eq!(out, "one\nTWO\nthree\nfour\nfive\nsix\nSEVEN\neight\n");
    }

    #[test]
    fn handles_pure_insertions_and_deletions() {
        let original = "a\nb\nc\n";
        let insert = "--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\n+INSERTED\n b\n";
        let out =
            apply_hunks(original, &parse_unified_diff(insert).unwrap()[0].hunks, "f").unwrap();
        assert_eq!(out, "a\nINSERTED\nb\nc\n");

        let delete = "--- a/f\n+++ b/f\n@@ -1,3 +1,2 @@\n a\n-b\n c\n";
        let out =
            apply_hunks(original, &parse_unified_diff(delete).unwrap()[0].hunks, "f").unwrap();
        assert_eq!(out, "a\nc\n");
    }

    #[test]
    fn preserves_absence_of_a_trailing_newline() {
        let original = "a\nb";
        let patch = "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n\\ No newline at end of file\n";
        let out = apply_hunks(original, &parse_unified_diff(patch).unwrap()[0].hunks, "f").unwrap();
        assert_eq!(out, "a\nB");
    }

    #[test]
    fn parses_multi_file_patches() {
        let patch = "--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-a\n+A\n\
                     --- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-b\n+B\n";
        let patches = parse_unified_diff(patch).unwrap();
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].path, "one.txt");
        assert_eq!(patches[1].path, "two.txt");
    }

    #[test]
    fn recognises_creation_and_deletion() {
        let create = "--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1,2 @@\n+fn new() {}\n+\n";
        let patches = parse_unified_diff(create).unwrap();
        assert_eq!(patches[0].action, PatchAction::Create);
        assert_eq!(patches[0].path, "new.rs");

        let delete = "--- a/old.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n";
        assert_eq!(
            parse_unified_diff(delete).unwrap()[0].action,
            PatchAction::Delete
        );
    }

    #[test]
    fn rejects_input_that_is_not_a_diff() {
        assert_eq!(
            parse_unified_diff("just some prose"),
            Err(PatchError::Empty)
        );
        assert!(matches!(
            parse_unified_diff("--- a/f\nnot a plus line\n"),
            Err(PatchError::Malformed(_))
        ));
    }

    #[test]
    fn ignores_git_preamble_lines() {
        let patch = "diff --git a/f.txt b/f.txt\n\
                     index 83db48f..bf269f4 100644\n\
                     --- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-a\n+A\n";
        let patches = parse_unified_diff(patch).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "f.txt");
    }

    // ---- end-to-end on a real directory --------------------------------------

    #[test]
    fn applies_to_disk_and_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
        let patch = "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n\
                     --- /dev/null\n+++ b/sub/new.txt\n@@ -0,0 +1 @@\n+created\n";

        let applied = apply_patch(dir.path(), patch).unwrap();
        assert_eq!(applied.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "a\nB\nc\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sub/new.txt")).unwrap(),
            "created\n"
        );
    }

    #[test]
    fn deletes_files() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("gone.txt");
        std::fs::write(&victim, "bye\n").unwrap();
        apply_patch(
            dir.path(),
            "--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
        )
        .unwrap();
        assert!(!victim.exists());
    }

    /// A patch that fails partway must not leave earlier files rewritten —
    /// otherwise the working tree ends up in a state neither the model nor the
    /// user can reason about.
    #[test]
    fn a_failing_hunk_leaves_every_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("first.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.path().join("second.txt"), "totally different\n").unwrap();
        let patch = "--- a/first.txt\n+++ b/first.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n\
                     --- a/second.txt\n+++ b/second.txt\n@@ -1,2 +1,2 @@\n x\n-y\n+Y\n";

        assert!(apply_patch(dir.path(), patch).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("first.txt")).unwrap(),
            "a\nb\n",
            "the first file must not be rewritten when a later hunk fails"
        );
    }

    #[test]
    fn missing_target_file_is_reported_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply_patch(dir.path(), SIMPLE).unwrap_err();
        assert!(matches!(err, PatchError::MissingFile(_)));
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
        apply_patch(
            dir.path(),
            "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-a\n+A\n",
        )
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("zcode-patch-tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
