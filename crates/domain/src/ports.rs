//! Port traits (interfaces) consumed by the application layer, plus the owned
//! message/value types that flow across each port. All pure (stdlib-only).

use std::path::{Path, PathBuf};

use crate::model::{AgentMode, ImageRef, Plugin, ShellCommand};

#[deprecated(note = "use LlmEvent instead; kept for v0.1 back-compat")]
#[allow(non_camel_case_types)]
#[derive(Clone, Debug)]
pub struct CompletionChunk {
    pub delta: String,
    pub done: bool,
}

/// A request to an LLM provider — the evolved message/history model (§4.1).
#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub messages: Box<[LlmMessage]>,
    pub tools: Box<[ToolSpec]>,
    pub model: String,
    pub max_tokens: u64,
    pub temperature: f32,
    pub images: Box<[ImageRef]>,
}

/// Role of a message in the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in the LLM conversation transcript.
#[derive(Clone, Debug)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    pub tool_calls: Box<[LlmToolCall]>,
    pub tool_result: Option<LlmToolResult>,
}

impl LlmMessage {
    pub fn system(text: &str) -> Self {
        Self {
            role: LlmRole::System,
            content: text.into(),
            tool_calls: Box::new([]),
            tool_result: None,
        }
    }

    pub fn user(text: &str) -> Self {
        Self {
            role: LlmRole::User,
            content: text.into(),
            tool_calls: Box::new([]),
            tool_result: None,
        }
    }

    pub fn assistant(text: &str) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: text.into(),
            tool_calls: Box::new([]),
            tool_result: None,
        }
    }

    pub fn tool_result_message(result: LlmToolResult) -> Self {
        Self {
            role: LlmRole::Tool,
            content: String::new(),
            tool_calls: Box::new([]),
            tool_result: Some(result),
        }
    }

    pub fn append_content(&mut self, fragment: &str) {
        self.content.push_str(fragment);
    }
}

/// A tool-call emitted by the assistant mid-generation.
#[derive(Clone, Debug)]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A tool result echoed back to the LLM as a `Tool`-role message.
#[derive(Clone, Debug)]
pub struct LlmToolResult {
    pub tool_call_id: String,
    pub content: String,
}

/// Why the model stopped generating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmFinishReason {
    Stop,
    ToolUse,
    Length,
}

/// Terminal event emitted at the end of a generation, carrying provider-reported
/// token usage (DQ2).
#[derive(Clone, Debug)]
pub struct LlmFinish {
    pub reason: LlmFinishReason,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
}

/// A streamed event from an LLM provider.
#[derive(Clone, Debug)]
pub enum LlmEvent {
    Delta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgs { id: String, arguments: String },
    Finish(LlmFinish),
}

/// Non-streaming aggregate response.
#[derive(Clone, Debug)]
pub struct LlmResponse {
    pub text: String,
    pub finish: LlmFinish,
    pub raw: String,
}

/// The LLM port: the application's only dependency on a model provider.
pub trait LlmPort {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, crate::BoxError>;
    fn stream(
        &mut self,
        req: &LlmRequest,
    ) -> Box<dyn Iterator<Item = Result<LlmEvent, crate::BoxError>> + Send>;
}

/// Specification of a tool the LLM may call (sent as a JSON-schema function).
#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub params_json: String,
}

/// Result of executing a tool. The engine turns this into an `LlmToolResult`
/// message and feeds it back to the LLM.
#[derive(Clone, Debug)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub error: Option<String>,
}

impl ToolResult {
    pub fn ok(content: &str) -> Self {
        Self {
            tool_call_id: String::new(),
            content: content.into(),
            error: None,
        }
    }

    pub fn from_tool_call_id(id: &str, content: String) -> Self {
        Self {
            tool_call_id: id.into(),
            content,
            error: None,
        }
    }

    pub fn err(tool_call_id: &str, message: &str) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: String::new(),
            error: Some(message.into()),
        }
    }
}

/// A native tool implementation (FR-C-01: add a tool by implementing `Tool`).
pub trait Tool {
    fn spec(&self) -> ToolSpec;
    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, crate::BoxError>;
}

/// Registry of all callable tools (native + MCP + LSP), presented as a single
/// namespace to the engine.
pub trait ToolRegistryPort {
    fn list(&self) -> Box<[ToolSpec]>;
    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, crate::BoxError>;
    fn is_native(&self, name: &str) -> bool;
}

/// A tool exposed by an MCP server.
#[derive(Clone, Debug)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

/// The MCP port (FR-MCP-01..05). Stdio JSON-RPC transport (DQ6).
pub trait McpPort {
    fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, crate::BoxError>;
    fn call(&mut self, name: &str, args_json: String) -> Result<String, crate::BoxError>;
    fn ping(&mut self) -> Result<bool, crate::BoxError>;
}

/// The LSP port (FR-LSP-01..04). `LspLocation`/`LspWorkspaceEdit` are domain-owned
/// types so `lsp-types` never leaks into domain (DQ7).
pub trait LspPort {
    fn goto_definition(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<crate::LspLocation, crate::BoxError>;

    fn find_references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Box<[crate::LspLocation]>, crate::BoxError>;

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<String, crate::BoxError>;

    fn rename_symbol(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<crate::LspWorkspaceEdit, crate::BoxError>;

    fn open_document(&mut self, uri: &str, text: &str) -> Result<(), crate::BoxError>;
}

/// A persisted agent session transcript.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub created_at: String,
    pub model: String,
    pub mode: AgentMode,
    pub last_message_at: String,
    pub step_count: u64,
    pub messages: Box<[LlmMessage]>,
}

/// Session store port (FR-SESSION-01..07, DQ9 UUIDv7).
pub trait SessionStorePort {
    fn create(&mut self) -> Result<String, crate::BoxError>;
    fn load(&self, id: &str) -> Result<Session, crate::BoxError>;
    fn checkpoint(&mut self, id: &str, session: &Session) -> Result<(), crate::BoxError>;
    fn fork(&mut self, id: &str, new_id: &str) -> Result<(), crate::BoxError>;
    fn import_from(&mut self, path: &Path) -> Result<String, crate::BoxError>;
    fn export_to(&self, id: &str, path: &Path) -> Result<(), crate::BoxError>;
}

/// A single serialization-bridge field carried by `TelemetryEvent.extra` so that
/// `domain` stays dep-free (no `serde_json` in domain — FR-DI-01).
#[derive(Clone, Debug)]
pub enum ExtraField {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Object(Box<[(String, ExtraField)]>),
    Array(Box<[ExtraField]>),
}

/// One telemetry event; the JSONL emitter turns these into one JSON line each.
#[derive(Clone, Debug)]
pub struct TelemetryEvent {
    pub kind: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
    pub steps: u64,
    pub execution_time_ms: u64,
    pub session_id: String,
    pub extra: Box<[(String, ExtraField)]>,
}

/// Accumulated totals written into `.zcode/reports/<ts>-<session>.json` (M1.7).
#[derive(Clone, Debug)]
pub struct TelemetryTotals {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
    pub steps: u64,
    pub execution_time_ms: u64,
    pub session_id: String,
    pub finish_reason: String,
    pub truncated: bool,
}

/// Telemetry port: stream JSONL events and flush a report file on completion.
pub trait TelemetryPort {
    fn emit(&mut self, ev: TelemetryEvent);
    fn flush_report(
        &mut self,
        session_id: &str,
        total: TelemetryTotals,
    ) -> Result<PathBuf, crate::BoxError>;
}

/// UI-facing events emitted by the engine for the renderer. In headless mode the
/// emitter is a no-op (the `TelemetryPort` writes JSONL instead).
#[derive(Clone, Debug)]
pub enum UiEvent {
    Delta(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgs {
        id: String,
        arguments: String,
    },
    ToolResult {
        tool_call_id: String,
        name: String,
        content: String,
        error: Option<String>,
    },
    Finish(LlmFinish),
    LoopStart {
        step: u64,
        max_turns: u64,
    },
    LoopEnd {
        steps: u64,
        finish_reason: LlmFinishReason,
        truncated: bool,
    },
    Error(String),
}

/// Rendering sink for engine events. Implemented by the JSONL writer, the pretty
/// stdout printer, and the TUI's channel bridge.
pub trait Emitter {
    fn emit(&mut self, ev: UiEvent);
}

pub trait FileSystemPort {
    fn read(&self, path: &Path) -> Result<String, crate::BoxError>;
    fn write(&self, path: &Path, content: &str) -> Result<(), crate::BoxError>;
    fn list(&self, path: &Path) -> Result<Vec<PathBuf>, crate::BoxError>;
    fn exists(&self, path: &Path) -> Result<bool, crate::BoxError>;
    fn watch(&self, _path: &Path) -> Result<crate::BoxError, crate::BoxError>;
}

pub trait ShellPort {
    fn spawn(&mut self, cmd: &ShellCommand) -> Result<(), crate::BoxError>;
    fn run(&mut self, cmd: &ShellCommand) -> Result<String, crate::BoxError>;
}

pub trait PluginRegistryPort {
    fn discover(&self) -> Result<Vec<Plugin>, crate::BoxError>;
    fn load(&self, plugin: &Plugin) -> Result<(), crate::BoxError>;
    fn execute(&self, plugin: &Plugin, input: &str) -> Result<String, crate::BoxError>;
}

pub trait LoggerPort {
    fn log(&self, level: LogLevel, msg: &str);
    fn with_field(&self, key: &str, value: &str) -> Box<dyn LoggerPort + Send + Sync>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_helpers_build_expected_roles() {
        let sys = LlmMessage::system("s");
        assert_eq!(sys.role, LlmRole::System);
        let usr = LlmMessage::user("u");
        assert_eq!(usr.role, LlmRole::User);
        let asst = LlmMessage::assistant("");
        assert_eq!(asst.role, LlmRole::Assistant);
        let t = LlmToolResult {
            tool_call_id: "c1".into(),
            content: "ok".into(),
        };
        let msg = LlmMessage::tool_result_message(t);
        assert_eq!(msg.role, LlmRole::Tool);
        assert!(msg.tool_result.is_some());
    }

    #[test]
    fn tool_result_helpers() {
        let r = ToolResult::ok("hi");
        assert!(r.error.is_none());
        let e = ToolResult::err("c1", "denied");
        assert_eq!(e.error.as_deref(), Some("denied"));
    }

    #[test]
    #[allow(deprecated)]
    fn completion_chunk_still_constructible() {
        let chunk = CompletionChunk {
            delta: String::new(),
            done: true,
        };
        assert!(chunk.done);
    }
}
