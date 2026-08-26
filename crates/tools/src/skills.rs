//! Skill discovery.
//!
//! A skill is a markdown note the agent can pull in as context — house style,
//! a review checklist, domain background. Two layouts are supported, because
//! both are in common use:
//!
//! ```text
//! skills/rust-style.md              a plain file
//! skills/rust-style/SKILL.md        a directory, as the Agent Skills convention uses
//! ```
//!
//! Several roots are searched (project first, then any configured or
//! machine-wide library), so a shared collection and per-project notes coexist.

use std::path::{Path, PathBuf};

/// One discovered skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    /// The name the model passes to the tool.
    pub name: String,
    pub path: PathBuf,
    /// One-line summary, for the tool description and `zcode skills list`.
    pub summary: String,
    /// Which root it came from.
    pub root: PathBuf,
}

/// Every skill visible to the agent, nearest root first.
#[derive(Debug, Clone, Default)]
pub struct SkillIndex {
    entries: Vec<SkillEntry>,
    roots: Vec<PathBuf>,
}

impl SkillIndex {
    /// Scan `roots` in order. A name found in an earlier root shadows the same
    /// name later, so a project can override a shared skill.
    pub fn discover(roots: &[PathBuf]) -> Self {
        let mut entries: Vec<SkillEntry> = Vec::new();
        for root in roots {
            let Ok(dir) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in dir.flatten() {
                let path = entry.path();
                let Some(found) = skill_at(&path) else {
                    continue;
                };
                if entries.iter().any(|e| e.name == found.0) {
                    continue; // an earlier root already provides this name
                }
                let summary = summarize(&found.1);
                entries.push(SkillEntry {
                    name: found.0,
                    path: found.1,
                    summary,
                    root: root.clone(),
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            entries,
            roots: roots.to_vec(),
        }
    }

    pub fn entries(&self) -> &[SkillEntry] {
        &self.entries
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Look up a skill by name, tolerating a `.md` suffix.
    pub fn get(&self, name: &str) -> Option<&SkillEntry> {
        let wanted = name.strip_suffix(".md").unwrap_or(name);
        self.entries.iter().find(|e| e.name == wanted)
    }
}

/// Resolve a directory entry to `(name, markdown path)` if it is a skill.
fn skill_at(path: &Path) -> Option<(String, PathBuf)> {
    if path.is_dir() {
        // `<name>/SKILL.md`, or `<name>/<name>.md` as a fallback.
        let name = path.file_name()?.to_str()?.to_string();
        if name.starts_with('.') {
            return None;
        }
        for candidate in ["SKILL.md", "skill.md"] {
            let file = path.join(candidate);
            if file.is_file() {
                return Some((name, file));
            }
        }
        let same_named = path.join(format!("{name}.md"));
        if same_named.is_file() {
            return Some((name, same_named));
        }
        return None;
    }
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return None;
    }
    let name = path.file_stem()?.to_str()?.to_string();
    // A README is documentation *about* the collection, not a skill.
    if name.starts_with('.') || name.eq_ignore_ascii_case("readme") {
        return None;
    }
    Some((name, path.to_path_buf()))
}

/// A one-line summary: the YAML front-matter `description` when present,
/// otherwise the first line of prose.
fn summarize(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    if let Some(description) = frontmatter_description(&text) {
        return description;
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("---"))
        .map(truncate_summary)
        .unwrap_or_default()
}

fn frontmatter_description(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let block = &rest[..end];
    for line in block.lines() {
        let Some(value) = line.trim().strip_prefix("description:") else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            return Some(truncate_summary(value));
        }
    }
    None
}

fn truncate_summary(text: &str) -> String {
    const MAX: usize = 160;
    let cleaned = text.trim();
    if cleaned.chars().count() <= MAX {
        return cleaned.to_string();
    }
    let mut out: String = cleaned.chars().take(MAX).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn finds_both_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("flat.md"), "# Flat\n\nA plain file skill.\n");
        // The Agent Skills convention: a directory holding SKILL.md.
        write(
            &root.join("nested/SKILL.md"),
            "# Nested\n\nA directory skill.\n",
        );

        let index = SkillIndex::discover(&[root]);
        assert_eq!(index.names(), vec!["flat", "nested"]);
        assert!(index.get("nested").unwrap().path.ends_with("SKILL.md"));
    }

    #[test]
    fn reads_the_frontmatter_description() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(
            &root.join("api/SKILL.md"),
            "---\nname: api\ndescription: Contract-first API design with protobuf.\n---\n\n# API\n",
        );
        let index = SkillIndex::discover(&[root]);
        assert_eq!(
            index.get("api").unwrap().summary,
            "Contract-first API design with protobuf."
        );
    }

    #[test]
    fn falls_back_to_the_first_line_of_prose() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(
            &root.join("style.md"),
            "# Style\n\nUse doc comments everywhere.\n",
        );
        let index = SkillIndex::discover(&[root]);
        assert_eq!(
            index.get("style").unwrap().summary,
            "Use doc comments everywhere."
        );
    }

    #[test]
    fn readme_and_hidden_entries_are_not_skills() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("README.md"), "About this collection\n");
        write(&root.join(".hidden.md"), "secret\n");
        write(&root.join(".git/config"), "[core]\n");
        write(&root.join("real.md"), "A real skill\n");

        let index = SkillIndex::discover(&[root]);
        assert_eq!(index.names(), vec!["real"]);
    }

    #[test]
    fn a_directory_without_markdown_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("empty-dir")).unwrap();
        assert!(SkillIndex::discover(&[root]).is_empty());
    }

    #[test]
    fn earlier_roots_shadow_later_ones() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let global = dir.path().join("global");
        write(&project.join("style.md"), "Project version\n");
        write(&global.join("style.md"), "Global version\n");
        write(&global.join("only-global.md"), "Global only\n");

        let index = SkillIndex::discover(&[project.clone(), global]);
        assert_eq!(index.names(), vec!["only-global", "style"]);
        // The project's copy wins.
        assert_eq!(index.get("style").unwrap().root, project);
        assert_eq!(index.get("style").unwrap().summary, "Project version");
    }

    #[test]
    fn missing_roots_are_ignored() {
        let index = SkillIndex::discover(&[PathBuf::from("/nonexistent/skills")]);
        assert!(index.is_empty());
    }

    #[test]
    fn lookup_tolerates_the_md_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("style.md"), "x\n");
        let index = SkillIndex::discover(&[root]);
        assert!(index.get("style.md").is_some());
        assert!(index.get("style").is_some());
        assert!(index.get("absent").is_none());
    }

    #[test]
    fn long_summaries_are_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("long.md"), &format!("{}\n", "word ".repeat(200)));
        let summary = SkillIndex::discover(&[root])
            .get("long")
            .unwrap()
            .summary
            .clone();
        assert!(summary.chars().count() <= 161, "{}", summary.len());
        assert!(summary.ends_with('…'));
    }
}
