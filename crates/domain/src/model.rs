use std::path::PathBuf;

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
