//! Search and discovery port (FR-SEARCH-*, FR-GLOB-*, FR-FILTER-*).
//!
//! The engine behind it is ripgrep's own (`infra-search`, PRD D-1); these are
//! the owned, stdlib-only types that cross the boundary. Paths are relative to
//! the project root and `/`-separated, so tool output is short and the same on
//! every platform; line numbers are 1-based (CE-DQ14).

use std::path::{Path, PathBuf};

/// How the case of a pattern is treated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseMode {
    /// Case-insensitive unless the pattern contains an upper-case letter.
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

/// One content search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepQuery {
    pub pattern: String,
    /// Treat `pattern` as a literal string, not a regex.
    pub literal: bool,
    /// Where to search: a directory or a single file.
    pub root: PathBuf,
    /// Only files matching one of these globs (empty = all).
    pub globs: Box<[String]>,
    /// ripgrep file types, e.g. `rust`, `ts`, `py` (empty = all).
    pub types: Box<[String]>,
    pub case: CaseMode,
    pub multiline: bool,
    /// Lines of context around each match (content mode).
    pub context: u8,
    /// Files larger than this are skipped and counted.
    pub max_file_bytes: u64,
    /// Stop collecting line detail after this many files; totals are still
    /// counted.
    pub max_files: usize,
    /// Matching lines kept per file.
    pub max_lines_per_file: usize,
}

impl GrepQuery {
    pub fn new(pattern: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            pattern: pattern.into(),
            literal: false,
            root: root.into(),
            globs: Box::new([]),
            types: Box::new([]),
            case: CaseMode::Smart,
            multiline: false,
            context: 0,
            max_file_bytes: 2_000_000,
            max_files: 1_000,
            max_lines_per_file: 10,
        }
    }
}

/// One line of a content result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepLine {
    /// 1-based.
    pub line: u32,
    pub text: String,
    /// A context line rather than a match.
    pub is_context: bool,
    /// 0-based byte column of the first match on the line.
    pub match_col: u32,
}

/// All matches in one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepFileHit {
    pub path: String,
    /// Matching lines in the file (may exceed `lines.len()`).
    pub count: u32,
    pub lines: Box<[GrepLine]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrepOutcome {
    /// Files with matches, sorted by path.
    pub files: Box<[GrepFileHit]>,
    /// Files that matched, including any beyond `max_files`.
    pub total_files: u64,
    /// Matching lines across all files.
    pub total_matches: u64,
    pub files_searched: u64,
    /// Files skipped for exceeding `max_file_bytes`.
    pub skipped_large: u64,
    /// The walk stopped early (cancelled, timed out, or hit the hard cap).
    pub partial: bool,
}

/// Whether a walk wants files, directories or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EntryKind {
    #[default]
    File,
    Dir,
    Any,
}

/// One filename search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobQuery {
    pub patterns: Box<[String]>,
    pub root: PathBuf,
    pub kind: EntryKind,
}

/// A path found by a walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkEntry {
    /// Relative to the project root, `/`-separated.
    pub path: String,
    pub is_dir: bool,
    /// Modification time, nanoseconds since the Unix epoch (0 if unknown).
    pub modified_ns: u128,
    /// A directory the discovery filter excludes, listed so the model knows
    /// it exists without seeing inside it (FR-FILTER-05).
    pub excluded: bool,
    /// Depth below the listed root (1 = direct child).
    pub depth: u8,
}

/// Why a path is excluded from discovery (FR-FILTER-07).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exclusion {
    /// The rule, as written (`node_modules/`, `build/`, `*.lock`).
    pub rule: String,
    /// Where it came from: `built-in`, `config`, or an ignore file and line.
    pub source: String,
}

/// The discovery engine. Every search, listing and index walk goes through
/// one implementation so no two tools disagree about what exists (PRD D-2).
pub trait SearchPort: Send + Sync {
    fn grep(
        &self,
        q: &GrepQuery,
        cancel: &dyn Fn() -> bool,
    ) -> Result<GrepOutcome, crate::BoxError>;
    fn glob(&self, q: &GlobQuery) -> Result<Box<[WalkEntry]>, crate::BoxError>;
    /// Entries below `root` to `depth` levels, sorted by path, with excluded
    /// directories included as collapsed entries.
    fn list(&self, root: &Path, depth: u8) -> Result<Box<[WalkEntry]>, crate::BoxError>;
    /// Every discoverable file below `root`, sorted by path.
    fn walk_files(&self, root: &Path) -> Result<Box<[WalkEntry]>, crate::BoxError>;
    /// Files worth indexing: [`SearchPort::walk_files`] minus binary,
    /// minified, generated and lock files (FR-FILTER-06).
    fn walk_files_for_index(&self, root: &Path) -> Result<Box<[WalkEntry]>, crate::BoxError> {
        self.walk_files(root)
    }
    /// Why `path` is excluded from discovery, if it is.
    fn explain(&self, path: &Path) -> Option<Exclusion>;
}
