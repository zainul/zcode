//! Discovery and search on ripgrep's own engine (PRD-CTX-EFF-003, D-1/D-2).
//!
//! [`RipgrepSearch`] implements `domain::SearchPort`. Everything that
//! discovers files — `grep`, `glob`, `list_dir`, the code index — goes through
//! one [`DiscoveryFilter`], so no two tools can disagree about what exists.
//!
//! Output is deterministic: walks may run in parallel, but results are always
//! sorted by path components before they leave this crate (FR-SEARCH-05).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]

pub mod defaults;
pub mod filter;
mod grep;
pub mod heuristics;

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use domain::{
    BoxError, EntryKind, Exclusion, GlobQuery, GrepOutcome, GrepQuery, SearchPort, WalkEntry,
};

pub use filter::{DiscoveryFilter, FilterConfig};

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// A pattern (regex, glob, or filter rule) that does not compile. The
    /// message is meant for the model: it says what to fix.
    #[error("invalid pattern: {0}")]
    Pattern(String),
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Compare `/`-separated paths component by component, so a directory's
/// entries sort together (`a/b` before `a-c`, which a byte compare reverses).
pub fn path_order(a: &str, b: &str) -> Ordering {
    a.split('/').cmp(b.split('/'))
}

/// Compile user globs. A bare `*.rs` matches at any depth, as ripgrep's
/// `--glob` and every editor's file finder do; `*` never crosses a `/`.
pub(crate) fn compile_globs(patterns: &[String]) -> Result<globset::GlobSet, SearchError> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = if pattern.contains('/') {
            pattern.clone()
        } else {
            format!("**/{pattern}")
        };
        builder.add(
            globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| SearchError::Pattern(format!("glob `{pattern}`: {e}")))?,
        );
    }
    builder
        .build()
        .map_err(|e| SearchError::Pattern(e.to_string()))
}

/// `path` relative to `root`, `/`-separated.
pub(crate) fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn modified_ns(meta: Option<std::fs::Metadata>) -> u128 {
    meta.and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

/// The `SearchPort` implementation.
#[derive(Clone, Debug)]
pub struct RipgrepSearch {
    filter: DiscoveryFilter,
}

impl RipgrepSearch {
    pub fn new(root: &Path, cfg: &FilterConfig) -> Result<Self, SearchError> {
        Ok(Self {
            filter: DiscoveryFilter::new(root, cfg)?,
        })
    }

    pub fn filter(&self) -> &DiscoveryFilter {
        &self.filter
    }

    fn entry(&self, path: &Path, is_dir: bool, depth: usize, excluded: bool) -> WalkEntry {
        WalkEntry {
            path: self.filter.relative(path),
            is_dir,
            modified_ns: modified_ns(std::fs::metadata(path).ok()),
            excluded,
            depth: u8::try_from(depth).unwrap_or(u8::MAX),
        }
    }

    fn sorted(mut entries: Vec<WalkEntry>) -> Box<[WalkEntry]> {
        entries.sort_by(|a, b| path_order(&a.path, &b.path));
        entries.dedup_by(|a, b| a.path == b.path);
        entries.into_boxed_slice()
    }

    fn walk(&self, root: &Path, kind: EntryKind) -> Result<Vec<(PathBuf, bool, usize)>, BoxError> {
        if !root.exists() {
            return Err(format!("{} does not exist", self.filter.relative(root)).into());
        }
        let mut out = Vec::new();
        for result in self.filter.walker(root).build() {
            let Ok(entry) = result else { continue };
            if entry.depth() == 0 && entry.file_type().is_some_and(|t| t.is_dir()) {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            let wanted = match kind {
                EntryKind::File => !is_dir,
                EntryKind::Dir => is_dir,
                EntryKind::Any => true,
            };
            if wanted {
                out.push((entry.path().to_path_buf(), is_dir, entry.depth()));
            }
        }
        Ok(out)
    }
}

impl SearchPort for RipgrepSearch {
    fn grep(&self, q: &GrepQuery, cancel: &dyn Fn() -> bool) -> Result<GrepOutcome, BoxError> {
        grep::grep(&self.filter, q, cancel).map_err(Into::into)
    }

    fn glob(&self, q: &GlobQuery) -> Result<Box<[WalkEntry]>, BoxError> {
        let set = compile_globs(&q.patterns)?;
        let entries = self
            .walk(&q.root, q.kind)?
            .into_iter()
            // Relative to the glob's root, so `src/**/*.rs` rooted at the
            // project and `**/*.rs` rooted at `src` both mean what they say.
            .filter(|(path, _, _)| set.is_match(relative_to(&q.root, path)))
            .map(|(p, is_dir, depth)| self.entry(&p, is_dir, depth, false))
            .collect();
        Ok(Self::sorted(entries))
    }

    fn list(&self, root: &Path, depth: u8) -> Result<Box<[WalkEntry]>, BoxError> {
        if !root.is_dir() {
            return Err(format!("{} is not a directory", self.filter.relative(root)).into());
        }
        let depth = usize::from(depth.max(1));
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut entries = Vec::new();
        let mut dirs_to_probe: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
        for result in self.filter.walker(root).max_depth(Some(depth)).build() {
            let Ok(entry) = result else { continue };
            if entry.depth() == 0 {
                continue;
            }
            let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
            seen.insert(entry.path().to_path_buf());
            if is_dir && entry.depth() < depth {
                dirs_to_probe.push((entry.path().to_path_buf(), entry.depth()));
            }
            entries.push(self.entry(entry.path(), is_dir, entry.depth(), false));
        }
        // FR-FILTER-05: a directory the filter hid is listed, collapsed, so
        // the model knows `node_modules/` exists without seeing inside it.
        for (dir, d) in dirs_to_probe {
            let Ok(children) = std::fs::read_dir(&dir) else {
                continue;
            };
            for child in children.flatten() {
                let path = child.path();
                let is_dir = child.file_type().is_ok_and(|t| t.is_dir());
                if is_dir && !seen.contains(&path) {
                    entries.push(self.entry(&path, true, d + 1, true));
                }
            }
        }
        Ok(Self::sorted(entries))
    }

    fn walk_files(&self, root: &Path) -> Result<Box<[WalkEntry]>, BoxError> {
        let entries = self
            .walk(root, EntryKind::File)?
            .into_iter()
            .map(|(p, is_dir, depth)| self.entry(&p, is_dir, depth, false))
            .collect();
        Ok(Self::sorted(entries))
    }

    fn walk_files_for_index(&self, root: &Path) -> Result<Box<[WalkEntry]>, BoxError> {
        let files = self.walk_files(root)?;
        Ok(files
            .into_vec()
            .into_iter()
            .filter(|e| heuristics::worth_indexing(&self.filter.root().join(&e.path)))
            .collect())
    }

    fn explain(&self, path: &Path) -> Option<Exclusion> {
        self.filter.explain(path)
    }
}

#[cfg(test)]
mod tests;
