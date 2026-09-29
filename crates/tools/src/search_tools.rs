//! Discovery tools on ripgrep's engine: `grep`, `glob`, and the `list_dir`
//! tree (FR-SEARCH-01..09, FR-GLOB-01..02, PRD §5.1/§5.5).
//!
//! All three are read-only, so every mode offers them — before this, the
//! only way to search in `planning`/`editing` was to read files whole (PRD
//! B3). Each returns the smallest useful answer first (paths before lines)
//! and ends a cut list with a footer naming the total and the next page.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use domain::{
    BoxError, CancelFlag, CaseMode, EntryKind, GlobQuery, GrepQuery, SearchPort, Subject, Tool,
    ToolResult, ToolSpec, WalkEntry,
};
use serde_json::Value;

use crate::native::{display_path, parse_args, resolve, tool_error};
use crate::render::{clip_line, count, digits, fnv64, gutter, Footer};

pub const TOOL_GREP: &str = "grep";
pub const TOOL_GLOB: &str = "glob";

/// The engine's cancel flag, set after the registry is built
/// (`ToolRegistryPort::set_cancel`) and read by a tool at call time.
pub type SharedCancel = Arc<Mutex<Option<CancelFlag>>>;

/// Shared handle to the search service (CE-DQ2).
pub type Search = Arc<dyn SearchPort + Send + Sync>;

/// Longest line a result shows (FR-SEARCH-04).
const LINE_CHARS: usize = 200;

fn string_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn usize_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn cancelled(cancel: &SharedCancel) -> bool {
    cancel
        .lock()
        .ok()
        .and_then(|c| c.as_ref().map(CancelFlag::triggered))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

pub struct GrepTool {
    root: PathBuf,
    search: Search,
    cancel: SharedCancel,
    max_file_bytes: u64,
    timeout: Duration,
}

impl GrepTool {
    pub fn new(root: PathBuf, search: Search, cancel: SharedCancel) -> Self {
        Self {
            root,
            search,
            cancel,
            max_file_bytes: 2_000_000,
            timeout: Duration::from_millis(10_000),
        }
    }

    pub fn with_limits(mut self, max_file_bytes: u64, timeout_ms: u64) -> Self {
        self.max_file_bytes = max_file_bytes;
        self.timeout = Duration::from_millis(timeout_ms.max(1));
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Files,
    Content,
    Count,
}

impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_GREP.into(),
            description: "Search file contents with ripgrep (regex; respects .gitignore; skips \
                          node_modules, build output and binaries). Start with output=files to \
                          find where, then output=content with a narrower path or glob to see \
                          lines. Line numbers are 1-based and work with read's offset."
                .into(),
            params_json: r#"{"type":"object","properties":{"pattern":{"type":"string","description":"Regex (Rust syntax), or a literal string with literal=true"},"literal":{"type":"boolean"},"path":{"type":"string","description":"File or directory to search; default: the project"},"glob":{"type":"array","items":{"type":"string"},"description":"Only files matching these globs, e.g. [\"*.rs\"]"},"type":{"type":"array","items":{"type":"string"},"description":"ripgrep file types, e.g. [\"rust\",\"ts\"]"},"case":{"type":"string","enum":["smart","sensitive","insensitive"]},"output":{"type":"string","enum":["files","content","count"],"description":"files (default): paths and match counts"},"context":{"type":"integer","minimum":0,"maximum":5},"multiline":{"type":"boolean"},"limit":{"type":"integer"},"offset":{"type":"integer"}},"required":["pattern"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let Some(pattern) = args
            .get("pattern")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
        else {
            return Ok(tool_error("missing required string argument `pattern`"));
        };
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = resolve(&self.root, path);
        let output = match args
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("files")
        {
            "files" => Output::Files,
            "content" => Output::Content,
            "count" => Output::Count,
            other => {
                return Ok(tool_error(format!(
                    "unknown output `{other}`; expected files, content or count"
                )))
            }
        };
        let case = match args.get("case").and_then(Value::as_str).unwrap_or("smart") {
            "smart" => CaseMode::Smart,
            "sensitive" => CaseMode::Sensitive,
            "insensitive" => CaseMode::Insensitive,
            other => {
                return Ok(tool_error(format!(
                    "unknown case `{other}`; expected smart, sensitive or insensitive"
                )))
            }
        };
        let offset = usize_arg(&args, "offset").unwrap_or(0);
        let limit = usize_arg(&args, "limit")
            .unwrap_or(if output == Output::Content { 100 } else { 50 })
            .max(1);
        let mut q = GrepQuery::new(pattern, root.clone());
        q.literal = bool_arg(&args, "literal");
        q.globs = string_list(&args, "glob").into_boxed_slice();
        q.types = string_list(&args, "type").into_boxed_slice();
        q.case = case;
        q.multiline = bool_arg(&args, "multiline");
        q.context = usize_arg(&args, "context").unwrap_or(0).min(5) as u8;
        q.max_file_bytes = self.max_file_bytes;
        // Line detail is only needed for the page being shown.
        q.max_files = if output == Output::Content {
            1_000
        } else {
            offset + limit
        };

        let started = Instant::now();
        let (cancel, timeout) = (self.cancel.clone(), self.timeout);
        let stop = move || cancelled(&cancel) || started.elapsed() > timeout;
        let outcome = match self.search.grep(&q, &stop) {
            Ok(o) => o,
            // A bad pattern, type or path is the model's to fix.
            Err(e) => return Ok(tool_error(e.to_string())),
        };
        let shown_path = display_path(&self.root, &root);
        let key = fnv64(
            format!(
                "{pattern}\0{}\0{:?}\0{:?}\0{:?}\0{}\0{}",
                shown_path, q.globs, q.types, output as u8, offset, limit
            )
            .as_bytes(),
        );

        let mut notes = Vec::new();
        if outcome.skipped_large > 0 {
            notes.push(format!(
                "{} over {} KB skipped",
                count(outcome.skipped_large, "file", "files"),
                self.max_file_bytes / 1_000
            ));
        }
        if outcome.partial {
            notes.push(if outcome.total_matches >= 100_000 {
                "stopped at 100000+ matches — narrow the pattern".to_string()
            } else {
                "partial: search stopped early (cancelled or timed out) — narrow with path, glob \
                 or type"
                    .to_string()
            });
        }

        if outcome.total_files == 0 {
            let mut text = format!(
                "no matches for /{pattern}/ in {shown_path} ({} searched)",
                count(outcome.files_searched, "file", "files")
            );
            if !notes.is_empty() {
                text.push_str(&format!(" [{}]", notes.join("; ")));
            }
            return Ok(ToolResult::ok(&text).with_subject(Subject::Search { key }));
        }

        let text = match output {
            Output::Files | Output::Count => {
                let total = outcome.files.len();
                let page = outcome.files.iter().skip(offset).take(limit);
                let mut out = String::new();
                for hit in page.clone() {
                    match output {
                        Output::Count => out.push_str(&format!("{}: {}\n", hit.path, hit.count)),
                        _ => out.push_str(&format!(
                            "{}  ({})\n",
                            hit.path,
                            count(u64::from(hit.count), "match", "matches")
                        )),
                    }
                }
                let shown = page.count();
                let next = (offset + shown < total).then_some(offset + shown);
                notes.insert(
                    0,
                    count(outcome.total_matches, "match", "matches") + " in total",
                );
                out.push_str(
                    &Footer {
                        shown,
                        total,
                        unit: "files",
                        next_offset: next,
                        hint: "narrow with `path`, `glob` or `type`",
                        notes,
                    }
                    .render(),
                );
                out
            }
            Output::Content => {
                // Page over matching lines (context lines ride along free).
                let items: Vec<(usize, &domain::GrepLine)> = outcome
                    .files
                    .iter()
                    .enumerate()
                    .flat_map(|(i, f)| f.lines.iter().map(move |l| (i, l)))
                    .collect();
                let match_positions: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, l))| !l.is_context)
                    .map(|(i, _)| i)
                    .collect();
                let total_lines = match_positions.len();
                let first = match_positions.get(offset).copied().unwrap_or(items.len());
                let last = match_positions
                    .get((offset + limit).min(total_lines).saturating_sub(1))
                    .copied()
                    .filter(|_| offset < total_lines);
                let mut out = String::new();
                let mut shown_matches = 0;
                if let Some(last) = last {
                    // Include the trailing context lines of the last match.
                    let mut end = last + 1;
                    while end < items.len()
                        && items[end].1.is_context
                        && items[end].0 == items[last].0
                    {
                        end += 1;
                    }
                    let width = items[first..end]
                        .iter()
                        .map(|(_, l)| digits(l.line))
                        .max()
                        .unwrap_or(1);
                    let mut current: Option<usize> = None;
                    for (file, line) in items[first..end].iter() {
                        if current != Some(*file) {
                            if let Some(prev) = current {
                                more_in_file(&mut out, &outcome.files[prev], true);
                            }
                            out.push_str(&outcome.files[*file].path);
                            out.push('\n');
                            current = Some(*file);
                        }
                        if !line.is_context {
                            shown_matches += 1;
                        }
                        out.push_str(&gutter(line.line, width, line.is_context));
                        out.push_str(&clip_line(&line.text, LINE_CHARS, line.match_col as usize));
                        out.push('\n');
                    }
                    if let Some(prev) = current {
                        let complete = end == items.len() || items[end].0 != prev;
                        more_in_file(&mut out, &outcome.files[prev], complete);
                    }
                }
                let next = (offset + shown_matches < total_lines).then_some(offset + shown_matches);
                notes.insert(
                    0,
                    format!(
                        "{} in {}",
                        count(outcome.total_matches, "match", "matches"),
                        count(outcome.total_files, "file", "files")
                    ),
                );
                out.push_str(
                    &Footer {
                        shown: shown_matches,
                        total: total_lines,
                        unit: "lines",
                        next_offset: next,
                        hint: "narrow with `path`, `glob` or `type`",
                        notes,
                    }
                    .render(),
                );
                out
            }
        };
        Ok(ToolResult::ok(&text).with_subject(Subject::Search { key }))
    }
}

/// `(+N more in this file)` when a file had more matches than were kept.
fn more_in_file(out: &mut String, file: &domain::GrepFileHit, complete: bool) {
    let kept = file.lines.iter().filter(|l| !l.is_context).count() as u32;
    if complete && file.count > kept {
        out.push_str(&format!("  (+{} more in this file)\n", file.count - kept));
    }
}

// ---------------------------------------------------------------------------
// glob
// ---------------------------------------------------------------------------

pub struct GlobTool {
    root: PathBuf,
    search: Search,
}

impl GlobTool {
    pub fn new(root: PathBuf, search: Search) -> Self {
        Self { root, search }
    }
}

impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_GLOB.into(),
            description: "Find files by name pattern (e.g. \"**/*.test.ts\"; a bare \"*.rs\" \
                          matches at any depth). Respects .gitignore and skips dependency and \
                          build directories."
                .into(),
            params_json: r#"{"type":"object","properties":{"pattern":{"type":"array","items":{"type":"string"}},"path":{"type":"string"},"sort":{"type":"string","enum":["path","modified"],"description":"modified: newest first"},"type":{"type":"string","enum":["file","dir","any"]},"limit":{"type":"integer"},"offset":{"type":"integer"}},"required":["pattern"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let patterns = string_list(&args, "pattern");
        if patterns.is_empty() {
            return Ok(tool_error("missing required argument `pattern`"));
        }
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = resolve(&self.root, path);
        let kind = match args.get("type").and_then(Value::as_str).unwrap_or("file") {
            "file" => EntryKind::File,
            "dir" => EntryKind::Dir,
            "any" => EntryKind::Any,
            other => {
                return Ok(tool_error(format!(
                    "unknown type `{other}`; expected file, dir or any"
                )))
            }
        };
        let by_modified = match args.get("sort").and_then(Value::as_str).unwrap_or("path") {
            "path" => false,
            "modified" => true,
            other => {
                return Ok(tool_error(format!(
                    "unknown sort `{other}`; expected path or modified"
                )))
            }
        };
        let offset = usize_arg(&args, "offset").unwrap_or(0);
        let limit = usize_arg(&args, "limit").unwrap_or(100).max(1);
        let q = GlobQuery {
            patterns: patterns.clone().into_boxed_slice(),
            root: root.clone(),
            kind,
        };
        let mut entries = match self.search.glob(&q) {
            Ok(e) => e.into_vec(),
            Err(e) => return Ok(tool_error(e.to_string())),
        };
        if by_modified {
            // Newest first, path breaking ties so the order is deterministic.
            entries.sort_by(|a, b| {
                b.modified_ns
                    .cmp(&a.modified_ns)
                    .then_with(|| a.path.cmp(&b.path))
            });
        }
        let shown_path = display_path(&self.root, &root);
        let subject = Subject::Listing {
            path: format!("{shown_path}::{}", patterns.join(",")),
        };
        if entries.is_empty() {
            return Ok(ToolResult::ok(&format!(
                "no paths match {} under {shown_path}",
                patterns.join(", ")
            ))
            .with_subject(subject));
        }
        let total = entries.len();
        let mut out = String::new();
        let mut shown = 0;
        for e in entries.iter().skip(offset).take(limit) {
            out.push_str(&e.path);
            if e.is_dir {
                out.push('/');
            }
            out.push('\n');
            shown += 1;
        }
        out.push_str(
            &Footer {
                shown,
                total,
                unit: "paths",
                next_offset: (offset + shown < total).then_some(offset + shown),
                hint: "narrow the pattern or `path`",
                notes: Vec::new(),
            }
            .render(),
        );
        Ok(ToolResult::ok(&out).with_subject(subject))
    }
}

// ---------------------------------------------------------------------------
// list_dir tree (FR-GLOB-02)
// ---------------------------------------------------------------------------

/// A directory with more children than this is summarised, not expanded.
pub const COLLAPSE_OVER: usize = 50;
/// Longest listing, in lines.
pub const MAX_LIST_LINES: usize = 200;

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn child_counts(dir: &Path) -> Option<(usize, usize)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let (mut files, mut dirs) = (0, 0);
    for e in entries.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            dirs += 1;
        } else {
            files += 1;
        }
    }
    Some((files, dirs))
}

fn counts_label(files: usize, dirs: usize) -> String {
    match (files, dirs) {
        (0, 0) => "(empty)".into(),
        (f, 0) => format!("({})", count(f as u64, "file", "files")),
        (0, d) => format!("({})", count(d as u64, "dir", "dirs")),
        (f, d) => format!(
            "({}, {})",
            count(f as u64, "file", "files"),
            count(d as u64, "dir", "dirs")
        ),
    }
}

/// A search service with only the built-in rules, for tools constructed
/// without one (tests, and `str_replace_editor` built on its own).
pub fn default_search(root: &Path) -> Search {
    Arc::new(
        infra_search::RipgrepSearch::new(root, &infra_search::FilterConfig::default())
            // The built-in rules are constants covered by infra-search's tests.
            .expect("built-in discovery rules compile"),
    )
}

/// Render `dir` (absolute) as an indented tree `depth` levels deep, through
/// the discovery filter. Shared by `list_dir` and `str_replace_editor
/// list_dir`.
pub fn list_tree(
    search: &dyn SearchPort,
    project_root: &Path,
    dir: &Path,
    depth: u8,
) -> Result<String, BoxError> {
    let depth = depth.clamp(1, 4);
    let entries: Vec<WalkEntry> = search.list(dir, depth)?.into_vec();
    let mut lines: Vec<String> = Vec::new();
    let mut collapsed: Vec<String> = Vec::new();
    for e in &entries {
        if collapsed.iter().any(|c| e.path.starts_with(c.as_str())) {
            continue;
        }
        let indent = "  ".repeat(usize::from(e.depth.saturating_sub(1)));
        let name = name_of(&e.path);
        let abs = project_root.join(&e.path);
        if e.excluded {
            lines.push(format!("{indent}{name}/  (excluded)"));
            continue;
        }
        if !e.is_dir {
            lines.push(format!("{indent}{name}"));
            continue;
        }
        let (files, dirs) = child_counts(&abs).unwrap_or((0, 0));
        if e.depth >= depth {
            // Not expanded at this depth: say how much is inside.
            lines.push(format!("{indent}{name}/  {}", counts_label(files, dirs)));
        } else if files + dirs > COLLAPSE_OVER {
            lines.push(format!("{indent}{name}/  {}", counts_label(files, dirs)));
            collapsed.push(format!("{}/", e.path));
        } else {
            lines.push(format!("{indent}{name}/"));
        }
    }
    let total = lines.len();
    let mut out = String::new();
    for line in lines.iter().take(MAX_LIST_LINES) {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(
        &Footer {
            shown: total.min(MAX_LIST_LINES),
            total,
            unit: "entries",
            next_offset: None,
            hint: "list a subdirectory, or a lower depth",
            notes: Vec::new(),
        }
        .render(),
    );
    Ok(out)
}

pub struct ListDirTool {
    root: PathBuf,
    search: Search,
}

impl ListDirTool {
    pub fn new(root: PathBuf, search: Search) -> Self {
        Self { root, search }
    }
}

impl Tool for ListDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: crate::native::TOOL_LIST_DIR.into(),
            description: "List a directory as a tree (depth 1-4). Dependency and build \
                          directories are shown collapsed as (excluded)."
                .into(),
            params_json: r#"{"type":"object","properties":{"path":{"type":"string"},"depth":{"type":"integer","minimum":1,"maximum":4}},"required":["path"]}"#.into(),
        }
    }

    fn call(&mut self, _name: &str, args_json: &str) -> Result<ToolResult, BoxError> {
        let args = match parse_args(args_json) {
            Ok(a) => a,
            Err(e) => return Ok(e),
        };
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let depth = usize_arg(&args, "depth").unwrap_or(1).clamp(1, 4) as u8;
        let full = resolve(&self.root, path);
        let shown = display_path(&self.root, &full);
        match list_tree(self.search.as_ref(), &self.root, &full, depth) {
            Ok(listing) => Ok(ToolResult::ok(&listing).with_subject(Subject::Listing {
                path: format!("{shown}@{depth}"),
            })),
            Err(e) => Ok(tool_error(format!("cannot list {shown}: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "src/lib.rs", "pub fn alpha() {}\n// needle one\n");
        write(
            r,
            "src/main.rs",
            "fn main() {\n    alpha();\n    // needle two\n    // needle three\n}\n",
        );
        write(r, "README.md", "needle in the readme\n");
        write(r, "node_modules/pkg/index.js", "needle in a dependency\n");
        dir
    }

    fn grep_tool(root: &Path) -> (GrepTool, SharedCancel) {
        let cancel = SharedCancel::default();
        let tool = GrepTool::new(root.to_path_buf(), default_search(root), cancel.clone());
        (tool, cancel)
    }

    fn run(tool: &mut dyn Tool, args: serde_json::Value) -> ToolResult {
        tool.call("x", &args.to_string()).unwrap()
    }

    #[test]
    fn files_mode_is_the_default_and_lists_paths_with_counts() {
        let dir = project();
        let (mut grep, _) = grep_tool(dir.path());
        let res = run(&mut grep, serde_json::json!({ "pattern": "needle" }));
        assert!(res.error.is_none(), "{res:?}");
        assert_eq!(
            res.content,
            "README.md  (1 match)\nsrc/lib.rs  (1 match)\nsrc/main.rs  (2 matches)\n\
             [3 files; 4 matches in total]"
        );
        assert!(!res.content.contains("node_modules"));
        assert!(matches!(res.subject, Some(Subject::Search { .. })));
    }

    #[test]
    fn content_mode_groups_by_file_with_a_one_based_gutter() {
        let dir = project();
        let (mut grep, _) = grep_tool(dir.path());
        let res = run(
            &mut grep,
            serde_json::json!({ "pattern": "needle", "output": "content", "path": "src" }),
        );
        assert_eq!(
            res.content,
            "src/lib.rs\n2│// needle one\nsrc/main.rs\n3│    // needle two\n4│    // needle three\n\
             [3 lines; 3 matches in 2 files]"
        );
    }

    #[test]
    fn content_mode_pages_over_matching_lines() {
        let dir = project();
        let (mut grep, _) = grep_tool(dir.path());
        let args = |offset| serde_json::json!({ "pattern": "needle", "output": "content", "limit": 2, "offset": offset });
        let first = run(&mut grep, args(0));
        assert!(first.content.ends_with("[showing 2 of 4 lines — narrow with `path`, `glob` or `type`; next page: offset 2; 4 matches in 3 files]"), "{}", first.content);
        let second = run(&mut grep, args(2));
        assert!(second.content.contains("needle two") && second.content.contains("needle three"));
        assert!(!second.content.contains("needle one"));
    }

    #[test]
    fn a_file_with_more_matches_than_kept_says_so() {
        let dir = project();
        write(dir.path(), "many.txt", &"needle\n".repeat(25));
        let (mut grep, _) = grep_tool(dir.path());
        let res = run(
            &mut grep,
            serde_json::json!({ "pattern": "needle", "output": "content", "path": "many.txt" }),
        );
        assert!(
            res.content.contains("(+15 more in this file)"),
            "{}",
            res.content
        );
    }

    #[test]
    fn count_mode_and_the_empty_result() {
        let dir = project();
        let (mut grep, _) = grep_tool(dir.path());
        let res = run(
            &mut grep,
            serde_json::json!({ "pattern": "needle", "output": "count" }),
        );
        assert!(res
            .content
            .starts_with("README.md: 1\nsrc/lib.rs: 1\nsrc/main.rs: 2\n"));
        let none = run(&mut grep, serde_json::json!({ "pattern": "zzz_absent" }));
        assert!(none.error.is_none());
        assert!(
            none.content.starts_with("no matches for /zzz_absent/ in"),
            "{}",
            none.content
        );
    }

    #[test]
    fn bad_arguments_are_errors_the_model_can_fix() {
        let dir = project();
        let (mut grep, _) = grep_tool(dir.path());
        for (args, expect) in [
            (serde_json::json!({}), "pattern"),
            (serde_json::json!({ "pattern": "a(" }), "invalid pattern"),
            (
                serde_json::json!({ "pattern": "a", "output": "lines" }),
                "unknown output",
            ),
            (
                serde_json::json!({ "pattern": "a", "type": ["cobol2000"] }),
                "invalid pattern",
            ),
        ] {
            let res = run(&mut grep, args.clone());
            let err = res.error.unwrap_or_default();
            assert!(err.contains(expect), "{args}: {err}");
        }
    }

    #[test]
    fn the_engines_cancel_flag_stops_a_search() {
        let dir = project();
        for i in 0..300 {
            write(dir.path(), &format!("bulk/{i}.txt"), "needle\n");
        }
        let (mut grep, cancel) = grep_tool(dir.path());
        let (flag, _) = CancelFlag::new();
        flag.trigger();
        *cancel.lock().unwrap() = Some(flag);
        let res = run(&mut grep, serde_json::json!({ "pattern": "needle" }));
        assert!(res.content.contains("partial"), "{}", res.content);
    }

    #[test]
    fn glob_lists_matches_sorted_with_paging() {
        let dir = project();
        let mut glob = GlobTool::new(dir.path().to_path_buf(), default_search(dir.path()));
        let res = run(&mut glob, serde_json::json!({ "pattern": ["*.rs"] }));
        assert_eq!(res.content, "src/lib.rs\nsrc/main.rs\n[2 paths]");
        let page = run(&mut glob, serde_json::json!({ "pattern": "*", "limit": 1 }));
        assert!(
            page.content.starts_with("README.md\n[showing 1 of 3 paths"),
            "{}",
            page.content
        );
        let none = run(&mut glob, serde_json::json!({ "pattern": "*.go" }));
        assert!(none.content.starts_with("no paths match *.go"));
        let dirs = run(
            &mut glob,
            serde_json::json!({ "pattern": "src", "type": "dir" }),
        );
        assert_eq!(dirs.content, "src/\n[1 paths]");
    }

    #[test]
    fn list_dir_renders_a_tree_with_excluded_directories_collapsed() {
        let dir = project();
        let mut list = ListDirTool::new(dir.path().to_path_buf(), default_search(dir.path()));
        let depth1 = run(&mut list, serde_json::json!({ "path": "." }));
        assert_eq!(
            depth1.content,
            "README.md\nnode_modules/  (excluded)\nsrc/  (2 files)\n[3 entries]"
        );
        let depth2 = run(&mut list, serde_json::json!({ "path": ".", "depth": 2 }));
        assert_eq!(
            depth2.content,
            "README.md\nnode_modules/  (excluded)\nsrc/\n  lib.rs\n  main.rs\n[5 entries]"
        );
        let missing = run(&mut list, serde_json::json!({ "path": "nope" }));
        assert!(missing.error.is_some());
    }

    #[test]
    fn a_huge_directory_is_summarised_not_expanded() {
        let dir = project();
        for i in 0..(COLLAPSE_OVER + 5) {
            write(dir.path(), &format!("gen/f{i:03}.txt"), "");
        }
        let mut list = ListDirTool::new(dir.path().to_path_buf(), default_search(dir.path()));
        let res = run(&mut list, serde_json::json!({ "path": ".", "depth": 3 }));
        assert!(res.content.contains("gen/  (55 files)"), "{}", res.content);
        assert!(!res.content.contains("f000.txt"));
    }
}
