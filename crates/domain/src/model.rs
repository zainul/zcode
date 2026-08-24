use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Ownership rule: all entity fields are owned (`String`/`PathBuf`/`Box<[T]>`)
/// to avoid lifetime propagation through use-cases (FR-PERF-03).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub constraints: Box<[String]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub old_content: String,
    pub new_content: String,
}

#[derive(Clone, Debug)]
pub struct ShellCommand {
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub entrypoint: PathBuf,
}

#[derive(Clone, Debug)]
pub struct AgentContext {
    pub working_dir: PathBuf,
    pub model: String,
    pub env: Vec<(String, String)>,
}

/// Agent operating mode (FR-MODE-01/02). `Planning` restricts the engine to
/// read-only tools; `Build` allows destructive/execute-side tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMode {
    Planning,
    Build,
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::Build
    }
}

impl std::str::FromStr for AgentMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "planning" | "Planning" => Ok(Self::Planning),
            "build" | "Build" => Ok(Self::Build),
            other => Err(format!("invalid agent mode: {other}")),
        }
    }
}

impl AgentMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Build => "build",
        }
    }
}

/// A base64-encoded image (data URI content / MIME) passed as vision input.
#[derive(Clone, Debug)]
pub struct ImageRef {
    pub mime: String,
    pub data: String,
}

/// A shared, thread-safe cancellation flag flipped by the CLI's signal handler
/// (FR-IFACE-05). The engine polls `triggered()` at turn boundaries.
#[derive(Clone)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> (Self, Self) {
        let flag = Arc::new(AtomicBool::new(false));
        (Self(flag.clone()), Self(flag))
    }

    pub fn triggered(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn trigger(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Default for CancelFlag {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

/// A location returned by an LSP server (line/character are 0-based per LSP spec).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspLocation {
    pub uri: String,
    pub range: LspRange,
}

/// A single text edit targeted at a document URI (from `textDocument/rename` etc.).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspTextEdit {
    pub uri: String,
    pub range: LspRange,
    pub new_text: String,
}

/// A workspace edit: a set of text edits to apply. The engine applies these via
/// the native `str_replace_editor` tool (FR-LSP-02: LSP is advice, FS tools are
/// the source of truth).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspWorkspaceEdit {
    pub changes: Box<[LspTextEdit]>,
}
