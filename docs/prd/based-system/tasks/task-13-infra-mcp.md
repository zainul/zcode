# Task 13 — Infra: MCP (Model Context Protocol) Client + McpPort

**Related PRD sections:** §3.3.2 MCP Integration (FR-MCP-01..05), §3.3 Extensible Tool System, §8 DQ6 (MCP transport)
**Depends on:** task-02 (Domain — `McpPort` trait defined in §4.3 of technical plan)
**Status:** To do
**Priority:** Medium (enables external data sources; graceful degradation keeps the agent usable without it)

## Objective

Implement a stdio JSON-RPC MCP client (`McpClient`) in `crates/infra/mcp` that satisfies `domain::McpPort`: discovers a server's tools via `tools/list` and routes agent tool calls via `tools/call`. Servers that fail to initialize are **logged and skipped** (FR-MCP-05) so the agent still runs with remaining tools. SSE transport is deferred to v0.3.0 (PRD §6 Out of Scope).

## Step-by-step

### 1. New crate `crates/infra/mcp`

`Cargo.toml`:
```toml
[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
serde = { workspace = true }
serde_json = { workspace = true }
[dev-dependencies]
assert-json-diff = "2.6"   # for fixture payloads
```

### 2. `src/lib.rs` — `McpClient`

```rust
pub struct McpClient {
    child: std::process::Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
    next_id: u64,
    logger: ...,  // LoggerPort or a sink
}
impl McpClient {
    pub fn new(command: &str, args: &[String], env: &[(String,String)]) -> Result<Self, McpError>;
    fn send_request(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, McpError>;
    fn read_message(&mut self) -> Result<serde_json::Value, McpError>;
}
impl McpPort for McpClient {
    fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, Box<dyn Error>> {
        let resp = self.send_request("tools/list", None)?;
        // map resp["tools"] -> McpToolDef { name, description, input_schema: json string }
    }
    fn call(&mut self, name: &str, args_json: String) -> Result<String, Box<dyn Error>> {
        let params = json!({ "name": name, "arguments": /* parse args_json */ });
        let resp = self.send_request("tools/call", Some(params))?;
        // join resp["content"] text blocks -> String
    }
    fn ping(&mut self) -> Result<bool, Box<dyn Error>> { self.send_request("ping", None).map(|_| true) }
}
```

### 3. Protocol details (stdlib JSON-RPC over stdio)

- `initialize` handshake: send `{"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}`, read ack; store `serverInfo`.
- `notifications/initialized` sent after init.
- `tools/list` → `tools/call` as above. Each request gets an incrementing numeric id; match responses by id.
- **Graceful degradation:** if `McpClient::new` spawn fails, or `initialize` times out (≥5 s via a deadline on `read_message`), the client records `McpStartupError`; the `ToolRegistry` (task-15) skips that server's tools and continues (FR-MCP-05). Never panics.

### 4. Tests

- `list_tools_parses_fixture`: spawn `echo` of a canned `tools/list` JSON (use a fake command `python3 -c "..."` or a tiny helper that writes a JSON then exits) — assert it maps to `McpToolDef` with name/description/schema.
- `call_parses_content_blocks`: canned `tools/call` response → joined content string.
- `spawn_failure_skipped`: point `command` at `/nonexistent` → `McpClient::new` returns `Err`; assert `McpPort` is never constructed, registry graceful-degrade path exercised.
- `initialize_timeout_returns_error`: a server that never writes the init ack → `ping()`/`list_tools()` returns error (not hang). Use a `timeout_ms` parameter derived from `config.timeout_ms`.

Integration (network/subprocess, `#[ignore]`):
- `mcp_everything_discovers_tools`: `#[ignore]`; run real `mcp-everything` server, assert ≥1 tool exposed (L2 / M1.9).

## Test-case scenario

- `ag.toml` defines `[[mcp.servers]] name="everything", command="npx", args=["-y","@modelcontextprotocol/server-everything"]`. On boot the `ToolRegistry` calls `list_tools` and merges an `everything/*` namespace of tools. `ag tools list` shows them. Killing the server mid-session degrades gracefully (logged, remaining tools usable).

## How to verify

```
cargo test -p infra-mcp
cargo test -p infra-mcp -- --ignored            # mcp-everything (needs npx)
cargo clippy -p infra-mcp -- -D warnings
cargo tree -p infra-mcp                          # must show only domain, serde, serde_json (+ assert-json-diff dev)
```

**Pass criteria:** stdio JSON-RPC round-trips `initialize`/`tools/list`/`tools/call`; a failing server is skipped (FR-MCP-05); zero `unsafe`; `cargo tree -p infra-mcp` lists only `{domain, serde, serde_json}`.

## Success metric mapping

- M1.9 (MCP ≥1 tool discovered, integration), M1.2/M1.3 (unit tests + lint), FR-MCP-01..05, DQ6 (stdio direct, no upstream crate), L1 provider-coverage is LLM not MCP; L2 ≥1 MCP server works end-to-end (integration). NFR-REL-04 (Child process dropped → killed).
