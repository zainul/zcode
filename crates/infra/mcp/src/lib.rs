//! MCP (Model Context Protocol) client over stdio JSON-RPC (FR-MCP-01..05, DQ6).
//!
//! The MCP stdio transport is **newline-delimited** JSON-RPC (one JSON object
//! per line) — unlike LSP, there is no `Content-Length` framing. A reader
//! thread pumps the child's stdout into a channel so every read can honour a
//! deadline: a server that never answers yields `McpError::Timeout` instead of
//! hanging the agent loop (FR-MCP-05).
//!
//! Direct deps: domain, serde_json. No async runtime, no upstream MCP crate.
#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use domain::{BoxError, McpPort, McpToolDef};
use serde_json::{json, Value};

/// Protocol revision we advertise in `initialize`.
const PROTOCOL_VERSION: &str = "2024-11-05";
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug)]
pub enum McpError {
    /// The server process could not be spawned (bad command, missing binary).
    Spawn(String),
    Io(std::io::Error),
    /// Malformed or unexpected JSON-RPC payload.
    Protocol(String),
    /// No response within the deadline — the server is wedged or slow.
    Timeout(u64),
    /// The server answered with a JSON-RPC `error` object.
    Server(String),
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "mcp spawn failed: {m}"),
            Self::Io(e) => write!(f, "mcp io error: {e}"),
            Self::Protocol(m) => write!(f, "mcp protocol error: {m}"),
            Self::Timeout(ms) => write!(f, "mcp timeout after {ms}ms"),
            Self::Server(m) => write!(f, "mcp server error: {m}"),
        }
    }
}

impl Error for McpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for McpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A live MCP server connection. Dropping the client kills the child process
/// (NFR-REL-04: no orphaned servers).
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
    timeout: Duration,
    server_name: String,
}

impl McpClient {
    /// Spawn a server and complete the `initialize` handshake.
    ///
    /// Returns `Err` (never panics) when the process cannot start or the
    /// handshake times out; the caller (`ToolRegistry`) logs and skips it so
    /// the agent still runs with the remaining tools (FR-MCP-05).
    pub fn new(command: &str, args: &[String], env: &[(String, String)]) -> Result<Self, McpError> {
        Self::with_timeout(command, args, env, DEFAULT_TIMEOUT_MS)
    }

    pub fn with_timeout(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        timeout_ms: u64,
    ) -> Result<Self, McpError> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Server logs go to stderr; keep them off the agent's own stdout
            // so `zcode run --json` stays valid JSONL (NFR-OBS-01).
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| McpError::Spawn(format!("{command}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn("no stdin pipe".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("no stdout pipe".into()))?;

        // Reader thread: line-delimited JSON into a channel so reads can time
        // out. It exits on EOF when the child dies.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut client = Self {
            child,
            stdin,
            rx,
            next_id: 0,
            timeout: Duration::from_millis(timeout_ms),
            server_name: command.to_string(),
        };
        client.handshake()?;
        Ok(client)
    }

    /// `initialize` request followed by the `notifications/initialized` ack.
    fn handshake(&mut self) -> Result<(), McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "clientInfo": { "name": "zcode", "version": env!("CARGO_PKG_VERSION") },
        });
        let result = self.send_request("initialize", Some(params))?;
        if let Some(name) = result
            .get("serverInfo")
            .and_then(|s| s.get("name"))
            .and_then(|n| n.as_str())
        {
            self.server_name = name.to_string();
        }
        self.send_notification("notifications/initialized", None)
    }

    /// Name reported by the server's `serverInfo` (falls back to the command).
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.write_line(&msg)
    }

    fn write_line(&mut self, msg: &Value) -> Result<(), McpError> {
        let line = serde_json::to_string(msg).map_err(|e| McpError::Protocol(e.to_string()))?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Send a request and block until the response with the matching id
    /// arrives (or the deadline expires). Notifications and unrelated ids are
    /// skipped, as required by JSON-RPC id matching.
    fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.write_line(&msg)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let value = self.read_message(deadline)?;
            match value.get("id").and_then(|v| v.as_u64()) {
                Some(got) if got == id => {
                    if let Some(err) = value.get("error") {
                        let text = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown error");
                        return Err(McpError::Server(text.to_string()));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                // Server-initiated request/notification or a stale id: ignore.
                _ => continue,
            }
        }
    }

    fn read_message(&mut self, deadline: Instant) -> Result<Value, McpError> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(McpError::Timeout(self.timeout.as_millis() as u64));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(v) => return Ok(v),
                        // Some servers print non-JSON banners; skip them.
                        Err(_) => continue,
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(McpError::Timeout(self.timeout.as_millis() as u64))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(McpError::Protocol("server closed stdout".into()))
                }
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Best-effort teardown; a server that already exited returns Err.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpPort for McpClient {
    fn list_tools(&mut self) -> Result<Box<[McpToolDef]>, BoxError> {
        let result = self.send_request("tools/list", None)?;
        Ok(parse_tool_defs(&result))
    }

    fn call(&mut self, name: &str, args_json: String) -> Result<String, BoxError> {
        // Arguments arrive as a JSON string from the LLM; forward them as a
        // real object (an unparseable payload becomes an empty object).
        let arguments: Value = serde_json::from_str(&args_json).unwrap_or_else(|_| json!({}));
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.send_request("tools/call", Some(params))?;
        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(Box::new(McpError::Server(join_content_blocks(&result))));
        }
        Ok(join_content_blocks(&result))
    }

    fn ping(&mut self) -> Result<bool, BoxError> {
        self.send_request("ping", None)?;
        Ok(true)
    }
}

/// Map a `tools/list` result into domain tool definitions.
pub fn parse_tool_defs(result: &Value) -> Box<[McpToolDef]> {
    let Some(tools) = result.get("tools").and_then(|t| t.as_array()) else {
        return Box::new([]);
    };
    tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "{}".to_string());
            Some(McpToolDef {
                name,
                description,
                input_schema,
            })
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Join the text blocks of a `tools/call` result into a single string.
/// Non-text blocks (images, resources) are summarised by their `type`.
pub fn join_content_blocks(result: &Value) -> String {
    let Some(blocks) = result.get("content").and_then(|c| c.as_array()) else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        let text = match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => block
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            Some(other) => format!("[{other} content omitted]"),
            None => continue,
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_defs_from_fixture() {
        let result = json!({
            "tools": [
                { "name": "echo", "description": "Echoes input",
                  "inputSchema": { "type": "object", "properties": { "msg": { "type": "string" } } } },
                { "name": "add" }
            ]
        });
        let defs = parse_tool_defs(&result);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "echo");
        assert_eq!(defs[0].description, "Echoes input");
        assert!(defs[0].input_schema.contains("properties"));
        // Missing description/schema degrade to empty/`{}` rather than failing.
        assert_eq!(defs[1].description, "");
        assert_eq!(defs[1].input_schema, "{}");
    }

    #[test]
    fn parse_tool_defs_tolerates_missing_tools_key() {
        assert!(parse_tool_defs(&json!({})).is_empty());
    }

    #[test]
    fn joins_content_blocks() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "image", "data": "..." },
                { "type": "text", "text": "line two" }
            ]
        });
        assert_eq!(
            join_content_blocks(&result),
            "line one\n[image content omitted]\nline two"
        );
    }

    #[test]
    fn spawn_failure_is_error_not_panic() {
        let Err(err) = McpClient::new("/nonexistent/mcp-server", &[], &[]) else {
            panic!("spawning a missing binary must fail");
        };
        assert!(matches!(err, McpError::Spawn(_)), "got {err:?}");
    }

    // ---- subprocess round-trips (Unix `sh` fixtures, hermetic: no network) ----

    #[cfg(unix)]
    fn fake_server(script: &str) -> Result<McpClient, McpError> {
        McpClient::with_timeout("sh", &["-c".into(), script.into()], &[], 5_000)
    }

    #[cfg(unix)]
    #[test]
    fn list_tools_round_trips_over_stdio() {
        // Replies to `initialize` (id 1) then `tools/list` (id 2); `cat` keeps
        // the process alive so our writes never hit EPIPE.
        let script = r#"printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"fake"}}}' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"d","inputSchema":{"type":"object"}}]}}'; cat > /dev/null"#;
        let mut client = fake_server(script).expect("handshake");
        assert_eq!(client.server_name(), "fake");
        let tools = client.list_tools().expect("tools/list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[cfg(unix)]
    #[test]
    fn call_returns_joined_content() {
        let script = r#"printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"hello"}]}}'; cat > /dev/null"#;
        let mut client = fake_server(script).expect("handshake");
        let out = client.call("echo", "{\"msg\":\"hi\"}".to_string()).unwrap();
        assert_eq!(out, "hello");
    }

    #[cfg(unix)]
    #[test]
    fn server_error_surfaces_as_error() {
        let script = r#"printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}' '{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"no such tool"}}'; cat > /dev/null"#;
        let mut client = fake_server(script).expect("handshake");
        let err = client.call("missing", "{}".to_string()).unwrap_err();
        assert!(err.to_string().contains("no such tool"));
    }

    #[cfg(unix)]
    #[test]
    fn initialize_timeout_returns_error_not_hang() {
        // Server never answers: handshake must give up on the deadline.
        let Err(err) =
            McpClient::with_timeout("sh", &["-c".into(), "cat > /dev/null".into()], &[], 300)
        else {
            panic!("a silent server must time out, not connect");
        };
        assert!(matches!(err, McpError::Timeout(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn unrelated_messages_are_skipped() {
        // A banner line, a notification, and a stale id precede the real reply.
        let script = r#"printf '%s\n' 'starting server...' '{"jsonrpc":"2.0","method":"notifications/message","params":{}}' '{"jsonrpc":"2.0","id":99,"result":{}}' '{"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"noisy"}}}'; cat > /dev/null"#;
        let client = fake_server(script).expect("handshake past noise");
        assert_eq!(client.server_name(), "noisy");
    }

    /// Live server integration (M1.9 / L2). Needs `npx` on PATH.
    #[test]
    #[ignore = "requires npx + network"]
    fn mcp_everything_discovers_tools() {
        let mut client = McpClient::with_timeout(
            "npx",
            &[
                "-y".into(),
                "@modelcontextprotocol/server-everything".into(),
            ],
            &[],
            30_000,
        )
        .expect("spawn mcp-everything");
        let tools = client.list_tools().expect("tools/list");
        assert!(!tools.is_empty(), "expected >= 1 tool");
    }
}
