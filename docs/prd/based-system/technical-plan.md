# Technical Plan: Minimal OpenCode-Capable Agent — QAgent (`ag`) v0.2.0

**Plan ID:** TP-AGENT-CORE-002
**Derived from:** `docs/prd/based-system/prd.md`
**Target:** QAgent v0.2.0 — first minimally functional end-to-end coding agent in Rust
**Lead Engineer:** Backend Team
**Status:** Draft (ready for implementation ordering per §11)

---

## 1. Executive Summary

v0.1.0 shipped a clean, **stubbed** skeleton: traits wired, `ag version` working, no I/O behavior. v0.2.0 turns the stubs into a **real, looping** coding agent that satisfies G1–G8 of the PRD. Concretely we deliver:

- A synchronous **engine loop** (`App::execute`) that renders chat history + tool specs to an LLM, streams deltas, dispatches `tool_use`, accumulates results, checkpoints, and iterates up to `max_turns`.
- An evolved **LLM port** that supports tool-calling and provider-reported token usage across OpenAI, Anthropic, OpenRouter, and Ollama (vLLM via OpenAI-compatible reuse).
- Two **interfaces** sharing one engine: a headless `ag run "<prompt>"` (JSONL to stdout) and an interactive `ratatui` TUI (`ag repl`) where the blocking loop runs on a worker thread and streams results to the renderer via an `mpsc` channel.
- An **extensible tool system** — native file/shell tools in `crates/tools`, plus MCP (stdio JSON-RPC) and LSP (rust-analyzer-style stdio JSON-RPC) as pluggable `ToolRegistryPort` backends.
- **Session + telemetry** persistence: `.ag/sessions/<uuidv7>.json` with auto-checkpoints, import/export JSON, and `.ag/reports/*.json` + JSONL streaming events.

The v0.1.0 architectural constraints are **frozen** (PRD §8): Domain is stdlib-only; App depends on Domain only; Infra depends on Domain; CLI is the composition root. All v0.2.0 changes honor this.

---

## 2. Resolved Architecture Decisions (from PRD §8 Open Questions + new)

| # | Question | Resolution | Rationale |
|---|----------|-----------|-----------|
| DQ1 | PTY / persistent shell | **Defer the persistent PTY shell.** `ShellPort::spawn` keeps returning `Pty(PtyError)` (Unix-only; Windows shim in v0.2.1) behind a `pty` cargo feature. The single-run `shell` tool via `run()` **is** fully implemented with the allowlist. Resolves PRD Q1; keeps M1.11 achievable without a heavy PTY dep. | Avoids a large native dep in the critical path; persistent shells are a v0.2.1 feature per FR-TOOL-SHELL-02's own "Stubbed in headless" note. |
| DQ2 | Token counting | **Provider-reported `usage`** is authoritative (LlmEvent::Finish carries `input_tokens`/`output_tokens`/`cache_tokens`). A whitespace/word heuristic in `domain::tokens` is the **fallback** only when a provider omits usage (e.g. some Ollama builds). Resolves PRD Q4. | Accuracy + cost attribution (FR-OUTPUT-03/04/05) without bloating domain with a tokenizer crate. |
| DQ3 | HTTP client | **`reqwest` blocking** (`features = ["blocking", "json", "rustls-tls", "gzip"]`) for all provider adapters, plus `serde_json` for SSE line parsing. Resolves PRD Q6. | Single client, rustls (no platform C toolchain for TLS), blocking form factor matches the sync ports (see DQ4) and keeps current-thread runtime unblocked. |
| DQ4 | Async vs sync ports | **Sync port traits** kept (Domain stdlib-only, async-agnostic). `LlmPort::stream` returns `Box<dyn Iterator<Item = LlmEvent>>`. Adapters do blocking HTTP reads; the TUI runs the engine on a **dedicated `std::thread`** with a `std::sync::mpsc` channel to the renderer. Headless `ag run` runs on the tokio current-thread directly (blocking is acceptable there). | Preserves FR-DI-01 (domain pure) and the frozen layering; avoids pulling `tokio`/`futures` into `domain` or `app`. |
| DQ5 | App port ownership | **`App` owns ports by value** (`Box<dyn Port + Send>`), dropping the v0.1 `Arc<dyn … + Sync>` wrapper. Single-owner, single-thread loop; `Send` suffices because the TUI moves the `App` into a worker `std::thread`. | Clean `&mut self` semantics (LlmPort/stream/mut call need unique access); removes the v0.1 Arc-mut mismatch. |
| DQ6 | MCP transport | **Stdio JSON-RPC implemented directly** in `crates/infra/mcp` (deps: `serde`, `serde_json`, `domain`). SSE transport deferred to v0.3.0 (PRD §6 Out of Scope #1 for v0.2). Resolves PRD Q3. | Eliminates dependency on an upstream crate whose 1.85 compatibility is uncertain; stdio covers `mcp-everything` and the vast majority of MCP servers. |
| DQ7 | LSP client | **`lsp-types` + hand-rolled JSON-RPC** over a spawned stdio process (deps: `lsp-types`, `serde_json`, `domain`). `tower-lsp` is **server-only**; not applicable as a client. Resolves PRD Q2. | Minimal dep footprint; stdio JSON-RPC is trivially synchronous and matches DQ4. |
| DQ8 | TUI framework | **`ratatui` 0.29** with `crossterm` backend (confirmed by PRD Q5). No extra features. | Matches the codebase's lean ethos; crossterm backend is Tier-1 on Linux/macOS. |
| DQ9 | Session ID format | **UUIDv7** via `uuid` `v7` feature (time-ordered, sortable). Resolves PRD Q7. | Deterministic ordering of sessions in `.ag/sessions/`, human-auditable. |
| DQ10 | Tool trait location | **`Tool` trait + `ToolSpec`/`ToolResult` + `ToolRegistryPort` trait live in `domain`** (pure). Concrete **native** tool impls (`read`, `write`, `str_replace`, `list_dir`, `shell`) and the merging **`ToolRegistry`** live in `crates/tools`. MCP/LSP tool adapters bridge into the same registry. | Domain stays the contract owner; app calls `ToolRegistryPort` without knowing native vs MCP vs LSP. Enforces FR-C-01 (add a tool = implement `Tool`). |
| DQ11 | Configuration shape | **Single `Config` serde struct** extended from v0.1 with nested `Provider`, `McpServer`, `LspServer`, `Shell` sections; `Loader` keeps env-over-file precedence. Secrets referenced by **name** (`api_key_env`) and resolved via `std::env::var` at `wire()`. | Satisfies FR-CONFIG-01–06, NFR-SEC-01 (keys never persisted). |

---

## 3. Crate Topology & Dependency Flow (v0.2.0)

Acyclic graph (direction = depends-on), enforced by `make check-deps`:

```
cli  ─────────────────────────────►  app   ──►  domain   (pure stdlib)
cli  ──►  infra/{llm, mcp, lsp,       (app holds only domain-side
        session, session, telemetry,   port traits; concrete impls
        filesystem, shell, config}      injected by cli)
cli  ──►  crates/tools  ──►  infra/{filesystem, shell, mcp, lsp, config}
cli  ──►  benches
```

New crates vs v0.1:

| Crate | Purpose | Deps (non-workspace) |
|-------|---------|----------------------|
| `crates/tools` | Native `Tool` impls + merging `ToolRegistry` | infra-filesystem, infra-shell, infra-mcp, infra-lsp, infra-config |
| `crates/infra/mcp` | Stdio JSON-RPC MCP client (`McpPort` impl) | serde_json |
| `crates/infra/lsp` | Stdio JSON-RPC LSP client (`LspPort` impl) | lsp-types, serde_json |
| `crates/infra/llm` | OpenAI/Anthropic/OpenRouter/Ollama adapters | reqwest, serde_json |
| `crates/infra/session` | UUIDv7 session store + import/export | uuid, serde_json |
| `crates/infra/telemetry` | JSONL emitter + report writer | serde_json |
| `crates/cli` | clap CLI + ratatui TUI + composition root | clap, tokio, ratatui, crossterm, uuid, log, env_logger |

`domain` and `app` gain **zero** third-party deps (FR-DI-01/02 hold).

---

## 4. Domain Layer Changes (freeze-respecting)

`crates/domain` stays stdlib-only. We **extend** it (not break the boundary) with new pure types and ports.

### 4.1 Evolved `LlmPort` (replaces the v0.1 stub)

The v0.1 `LlmPort` (`send(system, prompt)`, `stream(system, prompt)`) cannot express tool-calling or token usage. Evolve to a message/history model:

```rust
// domain/ports.rs  (add)
pub struct LlmRequest {
    pub messages: Box<[LlmMessage]>,
    pub tools: Box<[ToolSpec]>,
    pub model: String,
    pub max_tokens: u64,
    pub temperature: f32,
    pub images: Box<[ImageRef]>,        // base64 data-uris for vision
}
pub enum LlmRole { System, User, Assistant, Tool }
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    pub tool_calls: Box<[LlmToolCall]>,  // assistant messages
    pub tool_result: Option<ToolResult>, // tool messages
}
pub struct LlmToolCall { pub id: String, pub name: String, pub arguments: String }
pub struct ToolResult  { pub tool_call_id: String, pub content: String }
pub enum LlmEvent {
    Delta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgs { id: String, arguments: String },
    Finish(LlmFinish),
}
pub struct LlmFinish {
    pub reason: LlmFinishReason,   // Stop | ToolUse | Length
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_tokens: u64,
}
pub trait LlmPort {
    fn send(&mut self, req: &LlmRequest) -> Result<LlmResponse, Box<dyn Error>>;
    fn stream(&mut self, req: &LlmRequest)
        -> Box<dyn Iterator<Item = Result<LlmEvent, Box<dyn Error>>> + Send>;
}
pub struct LlmResponse { pub text: String, pub finish: LlmFinish, pub raw: String }
```

`CompletionChunk` is kept aliased for back-compat but the loop uses `LlmEvent`.

### 4.2 New `Tool` trait + `ToolRegistryPort`

```rust
pub struct ToolSpec { pub name: String, pub description: String, pub params_json: String } // JSON schema snippet
pub struct ToolResult { pub tool_call_id: String, pub content: String, pub error: Option<String> }
pub trait Tool {
    fn spec(&self) -> ToolSpec;
    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, Box<dyn Error>>;
}
pub trait ToolRegistryPort {
    fn list(&self) -> Box<[ToolSpec]>;
    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, Box<dyn Error>>;
    fn is_native(&self, name: &str) -> bool;     // planning mode uses this
}
```

### 4.3 New `McpPort` + `LspPort` (minimal surface)

```rust
pub struct McpToolDef { pub name: String, pub description: String, pub input_schema: String }
pub trait McpPort {
    fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, Box<dyn Error>>;
    fn call(&mut self, name: &str, args_json: String) -> Result<String, Box<dyn Error>>;
    fn ping(&mut self) -> Result<bool, Box<dyn Error>>;
}
pub trait LspPort {
    fn goto_definition(&mut self, uri: &str, line: u32, character: u32) -> Result<LspLocation, Box<dyn Error>>;
    fn find_references(&mut self, uri: &str, line: u32, character: u32) -> Result<Box<[LspLocation]>, Box<dyn Error>>;
    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<String, Box<dyn Error>>;
    fn rename_symbol(&mut self, uri: &str, line: u32, character: u32, new_name: &str) -> Result<LspWorkspaceEdit, Box<dyn Error>>;
    fn open_document(&mut self, uri: &str, text: &str) -> Result<(), Box<dyn Error>>;
}
```
`LspLocation`, `LspWorkspaceEdit` are tiny owned structs (serde-serializable) defined in domain; they avoid leaking `lsp-types` into domain.

### 4.4 New `SessionStorePort` + `TelemetryPort`

```rust
pub struct Session { pub id: String, pub created_at: String, pub model: String, pub mode: AgentMode, pub last_message_at: String, pub step_count: u64, pub messages: Box<[LlmMessage]> }
pub trait SessionStorePort {
    fn create(&mut self) -> Result<String, Box<dyn Error>>;            // returns UUIDv7
    fn load(&self, id: &str) -> Result<Session, Box<dyn Error>>;
    fn checkpoint(&mut self, id: &str, session: &Session) -> Result<(), Box<dyn Error>>;
    fn fork(&mut self, id: &str, new_id: &str) -> Result<(), Box<dyn Error>>;
    fn import_from(&mut self, path: &Path) -> Result<String, Box<dyn Error>>;
    fn export_to(&self, id: &str, path: &Path) -> Result<(), Box<dyn Error>>;
}
pub enum AgentMode { Planning, Build }
pub struct TelemetryEvent { pub kind: String, pub model: String, pub input_tokens: u64, pub output_tokens: u64, pub cache_tokens: u64, pub steps: u64, pub execution_time_ms: u64, pub session_id: String, pub extra: serde_json::Value }
pub trait TelemetryPort {
    fn emit(&mut self, ev: TelemetryEvent);
    fn flush_report(&mut self, session_id: &str, total: TelemetryTotals) -> Result<PathBuf, Box<dyn Error>>;
}
```
`TelemetryEvent.extra` uses `serde_json::Value` — but that makes `domain` depend on `serde_json` (a third-party crate), violating FR-DI-01. **Resolution**: define an enum `ExtraField { Text(String), Number(f64), Bool(bool), Null }` in domain and let the telemetry adapter render to JSON. Domain stays dep-free. (See task-19.)

### 4.5 New `ImageRef` + `LogLevel` stays

`ImageRef { mime: String, data: String }` (base64).

## 5. App Layer Changes (engine)

`crates/app` stays **domain-only** but gains the orchestration trait and loop.

```rust
pub struct ExecutionRequest {
    pub prompt: String,
    pub mode: AgentMode,
    pub session_id: Option<String>,
    pub images: Box<[ImageRef]>,
    pub max_turns: u64,
    pub max_tokens: u64,
    pub max_tool_output_chars: usize,
    pub stream: bool,
}
pub struct ExecutionResult {
    pub final_text: String,
    pub steps: u64,
    pub finish_reason: LlmFinishReason,
    pub truncated: bool,
}
pub trait AgentLoop {
    fn execute(&mut self, ctx: &AgentContext, req: ExecutionRequest) -> Result<ExecutionResult, AppError>;
}
pub struct App {
    llm: Box<dyn LlmPort + Send>,
    tools: Box<dyn ToolRegistryPort + Send>,
    sessions: Box<dyn SessionStorePort + Send>,
    telemetry: Box<dyn TelemetryPort + Send>,
    logger: Box<dyn LoggerPort + Send>,
}
```

`execute` implements the PRD §3.8 loop (render → stream → dispatch tool_use → checkpoint → repeat). It is **synchronous**; for `--json` headless it emits one JSON object per event to stdout (FR-OUTPUT-01). For planning mode it filters the tool set to read-only tools (FR-MODE-01).

`AppError` extends `Port(String)` and adds `Llm(String)`, `Tool(String)`, `Session(String)`, `Config(String)` variants.

## 6. High-Level Changes

### 6.1 Crate topology
Add the six new crates to `Cargo.toml` workspace `members`; pin `uuid`, `reqwest`, `serde_json`, `lsp-types`, `ratatui`, `crossterm` to `[workspace.dependencies]` (single source of truth; supports the deny.toml audit story).

### 6.2 Domain crate
Add `ports` extensions (§4.1–4.4), a pure `tokens` heuristic, and `AgentMode`/`Session` re-exports. **No new third-party deps.**

### 6.3 Application crate
Evolve `App` to own `LlmPort + ToolRegistryPort + SessionStorePort + TelemetryPort + LoggerPort`; implement `AgentLoop::execute`; drop the now-redundant `FileSystemPort`/`ShellPort`/`PluginRegistryPort` fields (FS/shell move inside tools; plugins → MCP).

### 6.4 Infrastructure crates
- `infra/llm`: replace stub with `OpenAiLlm`/`AnthropicLlm`/`OpenRouterLlm`/`OllamaLlm` reading the request JSON, streaming SSE, parsing `usage`.
- `infra/mcp` (new): spawn `[[mcp.servers]]` processes, send `initialize`/`tools/list`/`tools/call`, expose as `McpPort`.
- `infra/lsp` (new): spawn per-language LSP servers from `[lsp.servers.*]`, maintain an open-documents state, expose `LspPort`.
- `infra/filesystem` / `infra/shell`: unchanged interface; `shell` wrapped by a `GuardedShell` decorator applying the allowlist (FR-CONFIG-05). Native tools in `crates/tools` use these.
- `infra/config`: extend `Config` + `Loader` (task-20).
- `infra/session` (new): `UuidSessionStore` writing `.ag/sessions/<id>.json` with auto-checkpoints.
- `infra/telemetry` (new): `JsonTelemetry` emitting stdout JSONL + `.ag/reports/*.json`.

### 6.5 Tools crate
`crates/tools` defines `ToolRegistry` merging `Box<dyn Tool>` (native) + `McpPort`-backed + `LspPort`-backed tools behind `ToolRegistryPort`. Native tools: `read`, `write`, `str_replace_editor` (`view`/`create`/`str_replace`/`list_dir`), `shell` (guarded), `ag:skill` (read-only skills dir).

### 6.6 CLI crate (composition root)
- `wire(ctx)` reads `Config`, resolves API key by `api_key_env`, constructs the matching `LlmPort`, builds the `ToolRegistry` (native + configured MCP + LSP), wires session + telemetry ports, builds `App`.
- clap subcommands: `version`, `run`, `repl`, `session {create,continue,fork,import,export}`, `tools list`, `skills list`.
- `ag run` → headless `App::execute`, JSONL to stdout.
- `ag repl` → ratatui TUI; engine runs on `std::thread`, `mpsc<UiEvent>` back to renderer; `q`/`Ctrl-C` aborts, flushes telemetry + exports partial session (FR-IFACE-05).

## 7. Low-Level Changes (file-by-file)

### 7.1 Workspace `Cargo.toml`
```toml
members = [
  "crates/domain","crates/app","crates/tools",
  "crates/infra/llm","crates/infra/filesystem","crates/infra/shell",
  "crates/infra/config","crates/infra/mcp","crates/infra/lsp",
  "crates/infra/session","crates/infra/telemetry","crates/cli","benches",
]
[workspace.dependencies]
uuid = { version = "1.10", features = ["v7"] }
reqwest = { version = "0.12", default-features = false, features = ["blocking","json","rustls-tls","gzip"] }
serde_json = "1.0"
lsp-types = "0.97"
ratatui = "0.29"
crossterm = "0.28"
# existing: tokio, clap, toml, serde, thiserror, log, env_logger
```
`cli` keeps `#![forbid(unsafe_code)]`; `infra/shell` gains a `pty` feature gated with `#![cfg_attr(not(feature="pty"), forbid(unsafe_code))]` for the future PTY work.

### 7.2 `crates/domain/src/ports.rs`
Append (a) `LlmRequest`/`LlmEvent`/`LlmMessage`/`LlmFinish`/evolved `LlmPort`; (b) `Tool`+`ToolSpec`+`ToolResult`+`ToolRegistryPort`; (c) `McpPort`/`McpToolDef`; (d) `LspPort`/`LspLocation`/`LspWorkspaceEdit`; (e) `Session`/`SessionStorePort`; (f) `TelemetryEvent`/`TelemetryTotals`/`TelemetryPort`; (g) `AgentMode`/`ImageRef`. Re-export from `lib.rs`. Keep `CompletionChunk` as a deprecated alias.

### 7.3 `crates/domain/src/tokens.rs` (new, pure)
```rust
pub fn estimate_tokens(text: &str) -> u64 { text.split_whitespace().count() as u64 * 4 }
```
Used only as the provider-missing-usage fallback (DQ2).

### 7.4 `crates/app/src/lib.rs`
Redesign `App` to own the 5 ports (§5). `impl AgentLoop for App` with the `execute` loop. `AppError` gets `Llm`/`Tool`/`Session`/`Config` variants.

### 7.5 `crates/infra/llm/src/lib.rs` (evolved)
Replace stub body. Each provider impl:
```rust
pub struct OpenAiLlm { client: reqwest::blocking::Client, endpoint: String, api_key: String, model: String }
impl OpenAiLlm { pub fn new(endpoint:&str, api_key:&str, model:&str) -> Self }
impl LlmPort for OpenAiLlm {
    fn stream(&mut self, req:&LlmRequest) -> Box<dyn Iterator<Item=Result<LlmEvent>>+Send> {
        // POST to chat/completions with stream=true; iterate SSE lines,
        // parse delta|tool_call|finish_reason + usage, emit LlmEvent.
    }
}
// AnthropicLlm, OpenRouterLlm (reuse OpenAI shape w/ different endpoint),
// OllamaLlm (non-tools path; emits Finish with heuristic token estimate if no usage).
```
A private `sse_lines(resp)` helper yields lines from the blocking response.

### 7.6 `crates/infra/mcp/src/lib.rs` (new)
```rust
pub struct McpClient { child: Child, stdin: ChildStdin, reader: BufReader<ChildStdout>, next_id: u64 }
impl McpClient {
    pub fn new(command: &str, args: &[String], env: &[(String,String)]) -> Result<Self, McpError>;
    fn send_request(&mut self, method: &str, params: Value) -> Result<Value, McpError>;
}
impl McpPort for McpClient { ... tools/list maps to ToolSpec list ... }
```
Graceful degradation: a server that fails `initialize` is logged + skipped (FR-MCP-05), not fatal.

### 7.7 `crates/infra/lsp/src/lib.rs` (new)
```rust
pub struct LspClient { child: Child, stdin: ChildStdin, reader: BufReader<ChildStdout>, next_id: u60, docs: HashMap<String, String> }
impl LspClient { pub fn start(command:&str, args:&[String], lang_id:&str) -> Result<Self, LspError>; }
impl LspPort for LspClient { ... }
```
`open_document` keeps `docs` map and sends `textDocument/didOpen` + `didChange` on edits.

### 7.8 `crates/tools/src/lib.rs` (new)
`ToolRegistry { native: Vec<Box<dyn Tool>>, mcp: Vec<McpRef>, lsp: Option<LspRef> }` implementing `ToolRegistryPort`. Native tools: `FsReadTool`, `FsWriteTool`, `StrReplaceTool`, `ListDirTool`, `ShellTool` (delegating to a `GuardedShell`), `SkillTool`. `guarded_run` matches `cmd` segments against `shell.allowed` regexes (FR-CONFIG-05).

### 7.9 `crates/infra/session/src/lib.rs` (new)
`UuidSessionStore { base: PathBuf }` — `create()` writes empty `.ag/sessions/<v7>.json`, returns id; `checkpoint()` overwrites atomically (temp+rename); `fork`/`import`/`export` per FR-SESSION-01..07. Ignores `.ag` entries outside the sessions dir (FS-tool path traversal guarded too).

### 7.10 `crates/infra/telemetry/src/lib.rs` (new)
`JsonTelemetry { out: Box<dyn Write + Send>, report_dir: PathBuf }` — `emit()` writes one JSON object + newline; `flush_report()` serializes `TelemetryTotals` to `.ag/reports/<ts>-<session>.json`. `extra` field uses domain's `ExtraField` enum, rendered to JSON by `serde_json` only inside this crate (keeps domain pure).

### 7.11 `crates/infra/config/src/lib.rs` (extended, task-20)
Extend `Config` with `provider`, `model`, `api_key_env`, `max_turns`, `max_tokens`, `max_tool_output_chars`, `[[mcp.servers]]`, `[lsp.servers]`, `shell.allowed`, `skills_dir`, `mode`. `Loader` keeps env-over-file; `resolve_api_key` reads `api_key_env`.

### 7.12 `crates/cli/src/cli/mod.rs` (evolved)
- `Cli` derive with all subcommands (§6.6).
- `wire()` becomes `wire(&Config) -> Result<App, AppError>`: provider dispatch (FR-MODEL-06), constructs matching `LlmPort`, builds `ToolRegistry`, `UuidSessionStore`, `JsonTelemetry`.
- `ag run`: build `ExecutionRequest`, call `app.execute`, stream JSONL.
- `ag repl`: spawn ratatui app; spawn `std::thread` running `app.execute`; bridge via `mpsc`.
- `ag session *`, `ag tools list`, `ag skills list` delegate to ports.

### 7.13 `crates/cli/src/cli/tui.rs` (new)
ratatui render loop: message pane (top), tool-call/result pane (middle), input bar (bottom). Renders `UiEvent`s from the channel. `q`/`Esc`/`Ctrl-C` → signal abort.

### 7.14 `Makefile` + `deny.toml`
- New `check-deps` also asserts `cargo tree -p domain` and `-p app` have no third-party edges.
- `deny.toml` keeps license/advisory bans; `cargo audit` CI step added.

## 8. Testing Strategy & Verification Scenarios

| # | Scenario | Target | Method | Expected |
|---|----------|--------|--------|----------|
| T1 | Workspace compiles | workspace | `cargo build --workspace` | exit 0, 0 warnings |
| T2 | Domain pure | `domain` | `cargo tree -p domain` | no `[j`/cargo: lines (FR-DI-01) |
| T3 | App dep-free of 3rd party | `app` | `cargo tree -p app` | only `domain` + `thiserror` edges |
| T4 | Multi-provider dispatch | `infra-llm` | unit test `wire` with each provider string | constructs the right `Box<dyn LlmPort>` |
| T5 | LLM stub no network (Ollama local) | `infra-llm` | `#[ignore]` integration against local Ollama | streams a token |
| T6 | Shell allowlist allow | `crates/tools` | `ShellTool::call("echo hi")` | runs |
| T7 | Shell allowlist deny | `crates/tools` | `ShellTool::call("rm -rf /")` | `Err` refused (FR-CONFIG-04) |
| T8 | Shell deny-all | `crates/tools` | empty `allowed` → any cmd refused (M2.5) |
| T9 | str_replace atomic | `crates/tools` | tempdir, edit a file | old→new replaced, atomic via temp+rename |
| T10 | MCP tools/listed | `infra/mcp` | `#[ignore]` fixture `mcp-everything` | `tools list` shows an MCP tool (L2) |
| T11 | LSP goto def | `infra/lsp` | `#[ignore]` rust-analyzer fixture | resolves a definition (L3) |
| T12 | Session lifecycle | `infra/session` | create→checkpoint→fork→import→export | round-trip JSON, UUIDv7 format |
| T13 | Session auto-checkpoint | `infra/session` | kill mid-write → load | resumes from last completed step (NFR-REL-03) |
| T14 | JSONL emit | `infra/telemetry` | `ag run --json` | `jq -e .` parses every line (NFR-OBS-01) |
| T15 | Telemetry schema | `infra/telemetry` | assert report has required keys | M1.7 |
| T16 | Engine tool-use loop | `app` | fake `LlmPort` returning a `str_replace` tool_call → `ToolRegistry` mocks | file edited, step_count=1 |
| T17 | Planning mode read-only | `app` | planning mode, LLM returns `write` tool_call | engine refuses execute-side tool → `AppError::Tool` |
| T18 | TUI launches + aborts | `cli` | `ag repl` smoke (manual) | renders, `q` exits 0 |
| T19 | `ag run` end-to-end | `cli` | `#[ignore]` local Ollama, rename a var | file renamed (M1.5) |
| T20 | Version + build meta | `cli` | `cargo run -q -- version` | prints `ag v0.2.0 (git:…, profile:…)` |
| T21 | clippy + fmt | workspace | `cargo clippy --workspace -- -D warnings` / `cargo fmt --check` | exit 0 |
| T22 | Binary size | `cli` | `du -h target/release/ag` | < 12 MB (M2.4) |
| T23 | Cold start | `cli` | `time ./target/release/ag version` | < 300 ms (M2.1) |

All network/PTY/LSP/MCP/LLM-live tests are `#[ignore]`'d or gated behind a `network`/`integration` feature so `cargo test --workspace` is deterministic (NFR-REL-01). Hermetic tests use `tempfile` + in-process fakes.

## 9. Success Metrics & Acceptance Criteria (trace to PRD §6–7)

Primary gates (M1.*) — all must be green:
- M1.1 build 0 warnings; M1.2 tests green; M1.3 clippy+fmt; M1.4 acyclic graph; M1.5 e2e edit (local Ollama, `#[ignore]` but manually reproducible); M1.6 JSONL parseable; M1.7 report schema; M1.8 session lifecycle; M1.9 MCP discovers ≥1 tool; M1.10 allowlist deny; M1.11 TUI smoke.

Secondary (M2.*) — within threshold: cold <300 ms; single-turn edit (excl. network) <2 s; 20-step RSS <8 MB above baseline; release binary <12 MB; deny-all blocks; `make secrets-scan` = 0 hits.

Architecture gates (new for v0.2): `cargo tree -p domain`/`-p app` pure; `make check-deps` green; `cargo audit` clean (NFR-SEC-03).

## 10. Requirements Traceability (FR → file)

| FR ID | Satisfied by (task / file) |
|-------|----------------------------|
| FR-IFACE-01 / 02 / 03 / 04 / 05 / 06 | task-17 `cli/mod.rs`, `tui.rs`; `app::AgentLoop::execute` |
| FR-SESSION-01..07 | task-18 `infra/session` + `SessionStorePort` |
| FR-TOOL-FS-01..03 | task-15 `crates/tools` native `FsRead/FsWrite/StrReplace` tools |
| FR-TOOL-SHELL-01 / 02 | task-15 `ShellTool` + `GuardedShell`; PTY deferred (task-17 notes) |
| FR-MCP-01..05 | task-13 `infra/mcp` + `McpPort` |
| FR-LSP-01..04 | task-14 `infra/lsp` + `LspPort` |
| FR-MODEL-01..08 | task-12 `infra/llm` (4 providers) + `wire()` provider dispatch |
| FR-MODE-01..04 | task-16 planning/build mode in `execute` + `domain::modes` templates + config |
| FR-OUTPUT-01..09 | task-19 `infra/telemetry` + `ExecutionRequest`/`ExecutionResult` |
| FR-CONFIG-01..06 | task-20 `infra/config` + `.example.toml` |
| FR-LOOP-01..04 | task-16 `app::execute` turn/cap/truncation |
| FR (CLI matrix) | task-17 all subcommands |

## 11. Observability, Reliability, Stability & Security

**Observability** — `JsonTelemetry` emits one JSON event per LLM delta/tool-result/finish (NFR-OBS-01); `flush_report` writes `.ag/reports/<ts>-<session>.json` matching the documented schema (NFR-OBS-02). `LlmFinish` carries provider-reported token counts for cost attribution. `log`/`env_logger` wired in CLI for `RUST_LOG` debug; `LoggerPort` trait kept in domain for future structured adoption.

**Reliability** — `wire()` fails fast with typed `AppError::Config`/`AppError::Port` when a provider key or MCP server is misconfigured (NFR-REL-01, NFR-REL-02). `App::execute` wraps each turn in a checkpoint so a `Ctrl-C`/kill resumes from the last good step (NFR-REL-03, FR-SESSION-06). All child processes (MCP, LSP, shell, PTY) are held in RAII guards and `drop()`-killed on exit (NFR-REL-04). `cargo test` is hermetic by design (T1–T9, T12–T17, T20–T23 are pure; T10/T11/T5/T19 `#[ignore]`'d).

**Stability** — `panic = abort` + `lto = thin` + `codegen-units = 1` + `strip = symbols` (PRD NFR-PERF-05). `#[forbid(unsafe_code)]` in `cli` and `domain`; `unsafe` only in `infra/shell` behind the `pty` feature for termios (NFR-PORT-02). `deny.toml` bans copyleft; CI runs `cargo audit` (NFR-SEC-03). No `unsafe` in app/tools/telemetry/session/mcp (only `infra/shell` PTY path).

**Security** — secrets read **by name** from env at `wire()`, never written to disk (FR-CONFIG-03, NFR-SEC-01); `.ag/toml.local`, `.ag/skills`, `.ag/sessions`, `target/`, `*.profraw` already gitignored; `make secrets-scan` (T23) flags any committed key. Shell tool is gated by the `GuardedShell` allowlist decorator applying `shell.allowed` regex to **every** command segment, default-deny on empty list (NFR-SEC-02, FR-CONFIG-04/05). `ag:skill` path-traversal guarded (must resolve inside `skills_dir`). ratatui output is plain text; tool results are not interpreted as terminal escapes (NFR-SEC-04).

## 12. Implementation Roadmap (task ordering)

Dependencies between tasks drive the order. Config (task-20) + LLM port evolution (task-12) + domain ports + native tools/registry (task-15) + session (task-18) + telemetry (task-19) must land before the loop (task-16); MCP (task-13) and LSP (task-14) feed the registry and may run in parallel with task-15; TUI/CLI (task-17) is last.

```
task-20 (config)  ─┐
task-12 (llm)      ├─► task-16 (engine loop) ─► task-17 (TUI + run)
task-15 (tools)    ─┘        ▲
task-18 (session) ──────────┘
task-19 (telemetry)───────────┘
task-13 (mcp) ──► task-15  (parallel w/ 14)
task-14 (lsp) ──► task-15
```

Per-task details (step-by-step, test scenarios, verify commands, metric mapping) are in `docs/prd/based-system/tasks/task-{12,13,14,15,16,17,18,19,20}-*.md`.

---

*End of technical plan.*
