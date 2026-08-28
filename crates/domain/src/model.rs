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

/// Agent operating mode (FR-MODE-01/02).
///
/// Three rungs of autonomy, each a strict superset of the one before:
///
/// | mode       | read | edit files | run shell |
/// |------------|------|------------|-----------|
/// | `planning` | yes  | no         | no        |
/// | `editing`  | yes  | yes        | no        |
/// | `auto`     | yes  | yes        | yes       |
///
/// `editing` exists because "may rewrite my source" and "may execute arbitrary
/// commands" are genuinely different grants of trust: an agent that can edit a
/// file leaves a reviewable diff, one that can run a command does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMode {
    Planning,
    Editing,
    Auto,
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl std::str::FromStr for AgentMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "planning" | "plan" | "read-only" | "readonly" => Ok(Self::Planning),
            "editing" | "edit" => Ok(Self::Editing),
            // `build` is the v0.1 spelling of what is now `auto`; configs and
            // scripts in the wild still say it, so it keeps working.
            "auto" | "auto-run" | "autorun" | "build" => Ok(Self::Auto),
            other => Err(format!(
                "invalid agent mode: {other} (expected planning, editing, or auto)"
            )),
        }
    }
}

impl AgentMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "planning",
            Self::Editing => "editing",
            Self::Auto => "auto",
        }
    }

    /// Every mode, in increasing order of autonomy — the order `/mode` cycles
    /// through and the order help text lists them in.
    pub fn all() -> &'static [AgentMode] {
        &[Self::Planning, Self::Editing, Self::Auto]
    }

    /// The next mode in the cycle, so a single keystroke can step through them.
    pub fn next(self) -> Self {
        match self {
            Self::Planning => Self::Editing,
            Self::Editing => Self::Auto,
            Self::Auto => Self::Planning,
        }
    }

    /// One-line description for the TUI status bar and `--help`.
    pub fn summary(&self) -> &'static str {
        match self {
            Self::Planning => "read-only; proposes changes",
            Self::Editing => "edits files; no shell",
            Self::Auto => "edits files and runs shell",
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

    /// Wrap a flag owned by someone else — e.g. the `Arc<AtomicBool>` a
    /// signal handler writes to. Lets the CLI register SIGINT without the
    /// domain knowing anything about signals.
    pub fn from_shared(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// The underlying flag, for handing to an external registrar.
    pub fn shared(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }

    pub fn triggered(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn trigger(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Clear the flag. The REPL calls this after a cancelled turn so the next
    /// prompt is not aborted before it starts.
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
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
