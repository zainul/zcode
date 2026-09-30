//! Code-index port (FR-INDEX-*, technical plan §5.2).
//!
//! A *syntactic* index — definitions, imports and identifier occurrences per
//! file — built in the background and kept fresh. It answers "where is X" and
//! "what does this file contain" without reading files, and gives
//! `edit_symbol` exact spans. It is an accelerator and a source of
//! addresses; the language server stays the semantic authority.
//!
//! Line numbers here are **1-based**, like every model-facing number.

/// What a definition is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Type,
    Const,
    Module,
    /// A Rust `impl` block — a container, listed in outlines.
    Impl,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Type => "type",
            Self::Const => "const",
            Self::Module => "mod",
            Self::Impl => "impl",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "fn" | "function" | "func" | "def" => Self::Function,
            "method" => Self::Method,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "interface" => Self::Interface,
            "class" => Self::Class,
            "type" => Self::Type,
            "const" | "constant" | "var" => Self::Const,
            "mod" | "module" | "namespace" => Self::Module,
            "impl" => Self::Impl,
            _ => return None,
        })
    }
}

/// Where a definition sits. Lines are 1-based and inclusive; bytes are
/// offsets into the file as it was parsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
}

/// One definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolDef {
    pub name: String,
    /// With containers: `AgentLoop::execute`, `UserService.create`.
    pub qualified: String,
    pub kind: SymbolKind,
    /// Project-relative, `/`-separated.
    pub path: String,
    /// The whole definition, including attached doc comments, attributes
    /// and decorators.
    pub span: Span,
    /// The body block alone, when the definition has one (`replace_body`).
    pub body: Option<Span>,
    /// Where the name itself is (1-based line and character column).
    pub name_line: u32,
    pub name_col: u32,
    /// The definition's first line, trimmed and clipped.
    pub signature: String,
    /// Nesting depth: 0 top level, 1 inside a type or module, …
    pub depth: u8,
}

/// Whether the index can answer yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexState {
    Disabled,
    Building { done: u32, total: u32 },
    Ready,
}

/// A file's or a symbol's neighbourhood (FR-INDEX-07).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Related {
    /// What the file imports (raw specs, resolved to a path when possible).
    pub imports: Vec<String>,
    /// Files whose imports resolve to this one.
    pub imported_by: Vec<String>,
    /// For a symbol: where it is defined (`path:line`).
    pub defined_in: Vec<String>,
    /// Name-based, approximate: `path:line` where the name occurs.
    pub referenced_in: Vec<String>,
}

/// A file parsed on demand (not from the store) — what `edit_symbol` edits
/// against, so a stale index can never misplace an edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedFile {
    pub defs: Vec<SymbolDef>,
    /// `ERROR` and `MISSING` nodes in the tree.
    pub error_nodes: u32,
    /// Where the first of them is (1-based line, column).
    pub first_error: Option<(u32, u32)>,
}

/// The code index.
pub trait CodeIndexPort: Send + Sync {
    fn state(&self) -> IndexState;
    /// Definitions in `path` (or, for a directory, the top-level definitions
    /// of its files). `None`: no grammar for that language.
    fn outline(&self, path: &str) -> Result<Option<Vec<SymbolDef>>, crate::BoxError>;
    /// Definitions whose name matches `query` — exact, then prefix, then
    /// subsequence — optionally narrowed by kind and path prefix.
    fn symbols(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SymbolDef>, crate::BoxError>;
    /// Definitions named `qualified` (or ending in it), optionally in `path`.
    fn locate(
        &self,
        path: Option<&str>,
        qualified: &str,
    ) -> Result<Vec<SymbolDef>, crate::BoxError>;
    fn related(&self, target: &str) -> Result<Related, crate::BoxError>;
    /// Parse `text` as the language of `path`, bypassing the store.
    fn parse_text(&self, path: &str, text: &str) -> Result<Option<ParsedFile>, crate::BoxError>;
    /// A ranked, token-budgeted map of the repository (FR-INDEX-08).
    fn repo_map(&self, prompt: &str, budget_tokens: u32) -> Result<String, crate::BoxError>;
    /// Re-index `path` now (after zcode wrote it).
    fn notify_changed(&self, path: &str);
    /// Cheap check for files changed outside zcode, at the start of a turn.
    fn notify_turn_start(&self) {}
}
