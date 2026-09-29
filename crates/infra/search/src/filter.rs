//! The discovery filter: the single authority on what search, listing and
//! indexing can see (PRD D-2, FR-FILTER-01..05).
//!
//! Layers, in order:
//! 1. built-in excludes (`defaults.rs`),
//! 2. `.gitignore`, `.ignore`, `.git/info/exclude` and the global git
//!    excludes — honoured even outside a git repository,
//! 3. `.zcodeignore` files (gitignore syntax, nested like `.gitignore`),
//! 4. config `context.exclude`,
//! 5. config `context.include` and the built-in exceptions, which re-admit.
//!
//! Layers 1, 4 and 5 are evaluated here; 2 and 3 by the `ignore` crate's
//! walker, configured in [`DiscoveryFilter::walker`].
//!
//! **Explicit access is never filtered** (FR-FILTER-02). Directory rules match
//! a directory *by name* as the walk meets it, and the walk's own root is
//! never tested — so a walk started inside `node_modules/react` searches the
//! package, while a `.git` nested inside it is still pruned.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use domain::Exclusion;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::defaults::{ALWAYS_INCLUDED, EXCLUDED_DIRS, EXCLUDED_FILES, SECRET_FILES, VENV_MARKER};
use crate::SearchError;

/// User-supplied discovery rules (`[context] exclude/include`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterConfig {
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

/// The ignore-file names read at every directory level.
pub const IGNORE_FILES: &[&str] = &[".gitignore", ".ignore", ".zcodeignore"];

#[derive(Clone, Debug)]
struct Rule {
    /// As written, for `explain`.
    original: String,
    source: &'static str,
    dir_only: bool,
}

/// A compiled set of gitignore-style rules.
#[derive(Clone, Debug)]
struct RuleSet {
    set: GlobSet,
    rules: Vec<Rule>,
}

impl RuleSet {
    fn build(patterns: &[(String, &'static str)]) -> Result<Self, SearchError> {
        let mut builder = GlobSetBuilder::new();
        let mut rules = Vec::new();
        for (pattern, source) in patterns {
            for (glob, dir_only) in expand(pattern) {
                let compiled = GlobBuilder::new(&glob)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| SearchError::Pattern(format!("`{pattern}` ({source}): {e}")))?;
                builder.add(compiled);
                rules.push(Rule {
                    original: pattern.clone(),
                    source,
                    dir_only,
                });
            }
        }
        let set = builder
            .build()
            .map_err(|e| SearchError::Pattern(e.to_string()))?;
        Ok(Self { set, rules })
    }

    /// The first rule matching `rel` (respecting directory-only rules).
    fn first_match(&self, rel: &str, is_dir: bool) -> Option<&Rule> {
        self.set
            .matches(rel)
            .into_iter()
            .map(|i| &self.rules[i])
            .find(|r| is_dir || !r.dir_only)
    }
}

/// Turn one gitignore-style pattern into globs over root-relative paths.
///
/// No slash (or only a trailing one) matches at any depth; an inner slash
/// anchors to the root; a trailing slash means directories only. `dir/**`
/// also yields a directory rule for `dir` itself, so the walk prunes the
/// directory instead of visiting and rejecting every file below it.
fn expand(pattern: &str) -> Vec<(String, bool)> {
    let dir_only = pattern.ends_with('/');
    let trimmed = pattern.trim_end_matches('/');
    let anchored = trimmed.starts_with('/') || trimmed.contains('/');
    let body = trimmed.trim_start_matches('/');
    let glob = if anchored || body.starts_with("**/") {
        body.to_string()
    } else {
        format!("**/{body}")
    };
    let mut out = vec![(glob.clone(), dir_only)];
    if let Some(stem) = glob.strip_suffix("/**") {
        if !stem.is_empty() {
            out.push((stem.to_string(), true));
        }
    }
    out
}

/// Layers 1, 4 and 5, shared by every walker (`Arc`) and cheap to query.
#[derive(Debug)]
struct Matcher {
    excludes: RuleSet,
    includes: RuleSet,
}

impl Matcher {
    fn exclusion(&self, rel: &str, is_dir: bool, abs: &Path) -> Option<Exclusion> {
        if self.includes.first_match(rel, is_dir).is_some() {
            return None;
        }
        if is_dir && abs.file_name().is_some_and(|n| n == "env") && abs.join(VENV_MARKER).is_file()
        {
            return Some(Exclusion {
                rule: "env/ (a Python virtualenv)".into(),
                source: "built-in".into(),
            });
        }
        self.excludes
            .first_match(rel, is_dir)
            .map(|rule| Exclusion {
                rule: rule.original.clone(),
                source: rule.source.into(),
            })
    }
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct DiscoveryFilter {
    root: PathBuf,
    matcher: Arc<Matcher>,
}

impl DiscoveryFilter {
    pub fn new(root: &Path, cfg: &FilterConfig) -> Result<Self, SearchError> {
        let mut excludes: Vec<(String, &'static str)> = Vec::new();
        excludes.extend(EXCLUDED_DIRS.iter().map(|d| (format!("{d}/"), "built-in")));
        excludes.extend(EXCLUDED_FILES.iter().map(|f| (f.to_string(), "built-in")));
        excludes.extend(
            SECRET_FILES
                .iter()
                .map(|f| (f.to_string(), "built-in (secrets)")),
        );
        excludes.extend(
            cfg.exclude
                .iter()
                .map(|g| (g.clone(), "config context.exclude")),
        );
        let mut includes: Vec<(String, &'static str)> = Vec::new();
        includes.extend(ALWAYS_INCLUDED.iter().map(|f| (f.to_string(), "built-in")));
        includes.extend(
            cfg.include
                .iter()
                .map(|g| (g.clone(), "config context.include")),
        );
        Ok(Self {
            root: root.to_path_buf(),
            matcher: Arc::new(Matcher {
                excludes: RuleSet::build(&excludes)?,
                includes: RuleSet::build(&includes)?,
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `path` relative to the project root, `/`-separated; a path outside the
    /// root is returned whole, so rules still see its components.
    pub fn relative(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Layers 1, 4 and 5 only (built-in and config rules).
    pub fn rule_exclusion(&self, path: &Path, is_dir: bool) -> Option<Exclusion> {
        let rel = self.relative(path);
        if rel.is_empty() {
            return None;
        }
        self.matcher.exclusion(&rel, is_dir, path)
    }

    /// A walker rooted at `start` with every layer applied (FR-FILTER-01).
    pub fn walker(&self, start: &Path) -> ignore::WalkBuilder {
        let mut builder = ignore::WalkBuilder::new(start);
        builder
            .hidden(false) // FR-FILTER-04: dotfiles such as .github/ matter
            .parents(true)
            .ignore(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(false) // .gitignore applies outside a git repo too
            .follow_links(false) // FR-SEARCH-07
            .add_custom_ignore_filename(".zcodeignore");
        let matcher = Arc::clone(&self.matcher);
        let root = self.root.clone();
        builder.filter_entry(move |entry| {
            // The walk's own root is never filtered: naming a path is
            // explicit access (FR-FILTER-02).
            if entry.depth() == 0 {
                return true;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            let rel = entry.path().strip_prefix(&root).unwrap_or(entry.path());
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            matcher.exclusion(&rel, is_dir, entry.path()).is_none()
        });
        builder
    }

    /// Why `path` is hidden from discovery, if it is (FR-FILTER-07): a
    /// built-in or config rule, else the first ignore-file rule that matches
    /// it or one of its parents, nearest file first.
    pub fn explain(&self, path: &Path) -> Option<Exclusion> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        let is_dir = abs.is_dir();
        // A path is hidden when it or any ancestor below the root is.
        let mut chain: Vec<PathBuf> = abs
            .ancestors()
            .take_while(|a| a.starts_with(&self.root) && *a != self.root)
            .map(Path::to_path_buf)
            .collect();
        chain.reverse();
        for (i, p) in chain.iter().enumerate() {
            let dir = i + 1 < chain.len() || is_dir;
            if let Some(e) = self.rule_exclusion(p, dir) {
                return Some(e);
            }
        }
        let mut dir = abs.parent();
        while let Some(d) = dir {
            for name in IGNORE_FILES {
                let file = d.join(name);
                if !file.is_file() {
                    continue;
                }
                let (gi, _) = ignore::gitignore::Gitignore::new(&file);
                if let ignore::Match::Ignore(glob) = gi.matched_path_or_any_parents(&abs, is_dir) {
                    return Some(Exclusion {
                        rule: glob.original().to_string(),
                        source: self.relative(&file),
                    });
                }
            }
            if d == self.root {
                break;
            }
            dir = d.parent();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_style_patterns_expand_as_git_does() {
        assert_eq!(
            expand("node_modules/"),
            [("**/node_modules".to_string(), true)]
        );
        assert_eq!(expand("*.lock"), [("**/*.lock".to_string(), false)]);
        assert_eq!(expand("/build"), [("build".to_string(), false)]);
        assert_eq!(
            expand("vendor/**"),
            [
                ("vendor/**".to_string(), false),
                ("vendor".to_string(), true)
            ]
        );
        assert_eq!(expand("**/gen/"), [("**/gen".to_string(), true)]);
    }

    fn filter(cfg: FilterConfig) -> DiscoveryFilter {
        DiscoveryFilter::new(Path::new("/proj"), &cfg).unwrap()
    }

    #[test]
    fn built_in_rules_match_by_name_at_any_depth() {
        let f = filter(FilterConfig::default());
        let hidden = |p: &str, dir: bool| f.rule_exclusion(&Path::new("/proj").join(p), dir);
        assert!(hidden("node_modules", true).is_some());
        assert!(hidden("web/node_modules", true).is_some());
        assert!(hidden("Cargo.lock", false).is_some());
        assert!(hidden("a/b/app.min.js", false).is_some());
        // A *file* named like an excluded directory is not a directory.
        assert!(hidden("docs/build", false).is_none());
        assert!(hidden("src/main.rs", false).is_none());
    }

    #[test]
    fn secrets_are_excluded_except_the_documented_templates() {
        let f = filter(FilterConfig::default());
        let hidden = |p: &str| {
            f.rule_exclusion(&Path::new("/proj").join(p), false)
                .is_some()
        };
        assert!(hidden(".env"));
        assert!(hidden("api/.env.production"));
        assert!(hidden("certs/server.pem"));
        assert!(!hidden(".env.example"));
        assert!(!hidden(".vscode/settings.json"));
        assert!(hidden(".vscode/launch.json"));
    }

    #[test]
    fn config_include_readmits_and_exclude_adds() {
        let f = filter(FilterConfig {
            exclude: vec!["fixtures/".into(), "/generated/**".into()],
            include: vec!["vendor/**".into()],
        });
        let hidden = |p: &str, dir: bool| f.rule_exclusion(&Path::new("/proj").join(p), dir);
        assert!(
            hidden("vendor", true).is_none(),
            "include re-admits the directory"
        );
        assert!(hidden("vendor/lib.go", false).is_none());
        assert!(hidden("test/fixtures", true).is_some());
        assert!(
            hidden("generated", true).is_some(),
            "dir/** prunes the directory"
        );
        assert!(hidden("src/generated", true).is_none(), "leading / anchors");
        let e = hidden("test/fixtures", true).unwrap();
        assert_eq!(e.source, "config context.exclude");
        assert_eq!(e.rule, "fixtures/");
    }
}
