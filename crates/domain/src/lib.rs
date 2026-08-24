//! Domain layer — core business rules, entities, domain errors, and port traits.
//! Pure stdlib: zero third-party dependencies (enforced by FR-DI-01).
//!
//! The layer exposes:
//!  * **Entities / value objects** (`AgentContext`, `AgentMode`, `ImageRef`, `Session`,
//!    LSP location types) — owned structs with no lifetimes.
//!  * **Port traits** (`LlmPort`, `Tool`, `ToolRegistryPort`, `McpPort`, `LspPort`,
//!    `SessionStorePort`, `TelemetryPort`, `FileSystemPort`, `ShellPort`,
//!    `PluginRegistryPort`, `LoggerPort`, `Emitter`) and their associated
//!    message types (`LlmRequest`/`LlmEvent`, `ToolSpec`/`ToolResult`, …).
//!  * **Pure helpers** (`tokens::estimate_tokens`, `modes::system_prompt`).

pub mod error;
pub mod model;
pub mod modes;
pub mod ports;
pub mod tokens;

pub use error::DomainError;
pub use model::{
    AgentContext, AgentMode, CancelFlag, FileEdit, ImageRef, LspLocation, LspPosition, LspRange,
    LspTextEdit, LspWorkspaceEdit, Plugin, ShellCommand, Task, TaskStatus,
};
#[allow(deprecated)]
pub use ports::CompletionChunk;
pub use ports::{
    Emitter, ExtraField, FileSystemPort, LlmEvent, LlmFinish, LlmFinishReason, LlmMessage, LlmPort,
    LlmRequest, LlmResponse, LlmRole, LlmToolCall, LlmToolResult, LogLevel, LoggerPort, LspPort,
    McpPort, McpToolDef, PluginRegistryPort, Session, SessionStorePort, ShellPort, TelemetryEvent,
    TelemetryPort, TelemetryTotals, Tool, ToolRegistryPort, ToolResult, ToolSpec, UiEvent,
};

/// Shorthand for the canonical domain error box: `Send + Sync` so it crosses
/// thread boundaries (the engine runs on a worker thread in the TUI).
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
