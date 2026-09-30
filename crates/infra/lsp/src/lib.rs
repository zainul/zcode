//! LSP client over stdio JSON-RPC (FR-LSP-01..04, DQ7).
//!
//! Unlike MCP, the LSP wire format frames every message with a
//! `Content-Length` header, so a reader thread parses frames (not lines) into
//! a channel — which also lets every read honour a deadline instead of
//! hanging the agent loop when a language server stalls.
//!
//! `lsp-types` supplies the method-name constants (typo-proof, version-checked
//! at compile time); responses are mapped straight into **domain-owned**
//! `LspLocation`/`LspWorkspaceEdit` so `lsp-types` never leaks across the port
//! boundary into `domain` (FR-DI-01).
//!
//! Direct deps: domain, lsp-types, serde_json.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use domain::{
    BoxError, LspDiagnostic, LspLocation, LspPort, LspPosition, LspRange, LspReadiness,
    LspSymbolInfo, LspTextEdit, LspWorkspaceEdit,
};
use lsp_types::notification::{
    DidChangeTextDocument, DidOpenTextDocument, Initialized, Notification,
};
use lsp_types::request::{GotoDefinition, HoverRequest, Initialize, References, Rename, Request};
use serde_json::{json, Value};

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

mod pool;
pub use pool::{LspPool, ServerSpec, Starter};

#[derive(Debug)]
pub enum LspError {
    Spawn(String),
    Io(std::io::Error),
    Protocol(String),
    Timeout(u64),
    Server(String),
    /// The server answered, but with no usable result (e.g. no definition).
    NotFound(String),
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "lsp spawn failed: {m}"),
            Self::Io(e) => write!(f, "lsp io error: {e}"),
            Self::Protocol(m) => write!(f, "lsp protocol error: {m}"),
            Self::Timeout(ms) => write!(f, "lsp timeout after {ms}ms"),
            Self::Server(m) => write!(f, "lsp server error: {m}"),
            Self::NotFound(m) => write!(f, "lsp: {m}"),
        }
    }
}

impl Error for LspError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LspError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A live language-server connection with an in-memory document mirror.
/// Dropping it kills the child process (NFR-REL-04).
pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    /// Frames from the server, stamped with when they *arrived* — a report
    /// read late must not look newer than an edit that superseded it.
    rx: Receiver<(Value, Instant)>,
    next_id: u64,
    /// uri -> (text, version); the version is bumped on every `didChange`
    /// so the server's view stays in sync with our edits (FR-LSP-04).
    docs: HashMap<String, (String, i32)>,
    timeout: Duration,
    /// What the server said it can do, from `initialize` (CE-DQ19).
    capabilities: Value,
    /// Latest `publishDiagnostics` per document, with when it arrived.
    diagnostics: HashMap<String, (Vec<LspDiagnostic>, Instant)>,
    /// When each document was last changed, so a diagnostics read can wait
    /// for a report newer than the edit instead of returning a stale one.
    changed_at: HashMap<String, Instant>,
    /// When any diagnostics last arrived (the settle clock).
    last_push: Option<Instant>,
    /// `$/progress` tokens: `Some(pct)` while running, removed when done.
    progress: HashMap<String, Option<u8>>,
    /// Push-diagnostics settle window and cap (FR-LSP-07).
    settle: Duration,
    settle_cap: Duration,
}

impl LspClient {
    /// Spawn a language server rooted at `root_dir` and run the `initialize`
    /// handshake. Returns `Err` (never panics) if the binary is missing, so
    /// `wire()` can record the server as absent and continue.
    pub fn start(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        root_dir: &std::path::Path,
    ) -> Result<Self, LspError> {
        Self::start_with_timeout(command, args, env, root_dir, DEFAULT_TIMEOUT_MS)
    }

    pub fn start_with_timeout(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        root_dir: &std::path::Path,
        timeout_ms: u64,
    ) -> Result<Self, LspError> {
        let mut child = Command::new(command)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| LspError::Spawn(format!("{command}: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Spawn("no stdin pipe".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Spawn("no stdout pipe".into()))?;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(value) = read_frame(&mut reader) {
                if tx.send((value, Instant::now())).is_err() {
                    break;
                }
            }
        });

        let mut client = Self {
            child,
            stdin,
            rx,
            next_id: 0,
            docs: HashMap::new(),
            timeout: Duration::from_millis(timeout_ms),
            capabilities: Value::Null,
            diagnostics: HashMap::new(),
            changed_at: HashMap::new(),
            last_push: None,
            progress: HashMap::new(),
            settle: Duration::from_millis(1_500),
            settle_cap: Duration::from_secs(10),
        };
        client.handshake(root_dir)?;
        Ok(client)
    }

    fn handshake(&mut self, root_dir: &std::path::Path) -> Result<(), LspError> {
        let root_uri = path_to_uri(root_dir);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "definition": { "linkSupport": true },
                    "references": {},
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "rename": {},
                    "synchronization": { "didSave": false },
                    "publishDiagnostics": { "relatedInformation": false },
                    "diagnostic": { "dynamicRegistration": false }
                },
                "workspace": { "symbol": {}, "configuration": true },
                "window": { "workDoneProgress": true }
            },
            "clientInfo": { "name": "zcode", "version": env!("CARGO_PKG_VERSION") },
        });
        let result = self.send_request(Initialize::METHOD, params)?;
        self.capabilities = result.get("capabilities").cloned().unwrap_or(Value::Null);
        self.send_notification(Initialized::METHOD, json!({}))
    }

    /// Test/inspection accessor for the mirrored document set.
    pub fn documents(&self) -> &HashMap<String, (String, i32)> {
        &self.docs
    }

    fn write_message(&mut self, msg: &Value) -> Result<(), LspError> {
        let body = serde_json::to_string(msg).map_err(|e| LspError::Protocol(e.to_string()))?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())?;
        self.stdin.write_all(body.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }

    fn send_notification(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_message(&msg)
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_message(&msg)?;

        let deadline = Instant::now() + self.timeout;
        loop {
            let (value, arrived) = self.read_message(deadline)?;
            match value.get("id").and_then(|v| v.as_u64()) {
                Some(got) if got == id => {
                    if let Some(err) = value.get("error") {
                        let text = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown error");
                        return Err(LspError::Server(text.to_string()));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                // Diagnostics, progress and server->client requests share
                // the stream. They used to be skipped — including requests a
                // server waits on (`workspace/configuration`), which could
                // stall it. Now each is handled (CE-DQ19).
                _ => self.route(&value, arrived)?,
            }
        }
    }

    /// Handle one message that is not the response being awaited.
    fn route(&mut self, v: &Value, arrived: Instant) -> Result<(), LspError> {
        let method = v.get("method").and_then(Value::as_str);
        match (v.get("id"), method) {
            (Some(id), Some(method)) => {
                self.answer_server_request(id.clone(), method, v.get("params"))
            }
            (None, Some("textDocument/publishDiagnostics")) => {
                if let Some(params) = v.get("params") {
                    let uri = params["uri"].as_str().unwrap_or_default().to_string();
                    let items = parse_diagnostics(&uri, &params["diagnostics"]);
                    self.diagnostics.insert(uri, (items, arrived));
                    self.last_push = Some(arrived);
                }
                Ok(())
            }
            (None, Some("$/progress")) => {
                if let Some(params) = v.get("params") {
                    let token = params["token"].to_string();
                    let value = &params["value"];
                    let pct = value["percentage"].as_u64().map(|p| p.min(100) as u8);
                    match value["kind"].as_str() {
                        Some("end") => {
                            self.progress.remove(&token);
                        }
                        Some(_) => {
                            let entry = self.progress.entry(token).or_insert(None);
                            if pct.is_some() {
                                *entry = pct;
                            }
                        }
                        None => {}
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Reply to a server→client request. zcode applies edits itself, so it
    /// declines `workspace/applyEdit`; anything unknown gets the JSON-RPC
    /// "method not found" error rather than silence.
    fn answer_server_request(
        &mut self,
        id: Value,
        method: &str,
        params: Option<&Value>,
    ) -> Result<(), LspError> {
        let result = match method {
            "workspace/configuration" => {
                let n = params
                    .and_then(|p| p["items"].as_array())
                    .map_or(0, Vec::len);
                Value::Array(vec![Value::Null; n])
            }
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
            | "window/showMessageRequest" => Value::Null,
            "workspace/applyEdit" => json!({ "applied": false }),
            _ => {
                return self.write_message(&json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "method not supported by zcode" }
                }))
            }
        };
        self.write_message(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// Route whatever arrives until `until`.
    fn pump(&mut self, until: Instant) -> Result<(), LspError> {
        while Instant::now() < until {
            match self
                .rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok((v, arrived)) => {
                    // A response nobody is waiting for is dropped.
                    if v.get("method").is_some() {
                        self.route(&v, arrived)?;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(LspError::Protocol("server closed stdout".into()))
                }
            }
        }
        Ok(())
    }

    /// Tune the push-diagnostics wait (tests use short windows).
    pub fn set_diagnostics_settle(&mut self, settle: Duration, cap: Duration) {
        self.settle = settle;
        self.settle_cap = cap;
    }

    fn stored(&self, uri: Option<&str>) -> Box<[LspDiagnostic]> {
        let mut out: Vec<LspDiagnostic> = match uri {
            Some(u) => self
                .diagnostics
                .get(u)
                .map(|(items, _)| items.clone())
                .unwrap_or_default(),
            None => self
                .diagnostics
                .iter()
                .filter(|(u, _)| self.docs.contains_key(*u))
                .flat_map(|(_, (items, _))| items.clone())
                .collect(),
        };
        out.sort_by(|a, b| {
            (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
                b.uri.as_str(),
                b.range.start.line,
                b.range.start.character,
            ))
        });
        out.into_boxed_slice()
    }

    fn read_message(&mut self, deadline: Instant) -> Result<(Value, Instant), LspError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(LspError::Timeout(self.timeout.as_millis() as u64));
        }
        match self.rx.recv_timeout(remaining) {
            Ok(v) => Ok(v),
            Err(RecvTimeoutError::Timeout) => {
                Err(LspError::Timeout(self.timeout.as_millis() as u64))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(LspError::Protocol("server closed stdout".into()))
            }
        }
    }

    fn position_params(uri: &str, line: u32, character: u32) -> Value {
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        })
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl LspPort for LspClient {
    fn goto_definition(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<LspLocation, BoxError> {
        let result = self.send_request(
            GotoDefinition::METHOD,
            Self::position_params(uri, line, character),
        )?;
        parse_location(&result)
            .ok_or_else(|| Box::new(LspError::NotFound("no definition found".into())) as BoxError)
    }

    fn find_references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Box<[LspLocation]>, BoxError> {
        let mut params = Self::position_params(uri, line, character);
        params["context"] = json!({ "includeDeclaration": false });
        let result = self.send_request(References::METHOD, params)?;
        Ok(parse_locations(&result))
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Result<String, BoxError> {
        let result = self.send_request(
            HoverRequest::METHOD,
            Self::position_params(uri, line, character),
        )?;
        Ok(parse_hover(&result))
    }

    fn rename_symbol(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<LspWorkspaceEdit, BoxError> {
        let mut params = Self::position_params(uri, line, character);
        params["newName"] = json!(new_name);
        let result = self.send_request(Rename::METHOD, params)?;
        Ok(parse_workspace_edit(&result))
    }

    /// Mirror a document and push it to the server. First call sends
    /// `didOpen`; later calls send `didChange` with the full new text so the
    /// server's index tracks our edits (FR-LSP-04).
    fn open_document(&mut self, uri: &str, text: &str) -> Result<(), BoxError> {
        self.changed_at.insert(uri.to_string(), Instant::now());
        match self.docs.get_mut(uri) {
            Some(entry) => {
                entry.0 = text.to_string();
                entry.1 += 1;
                let version = entry.1;
                self.send_notification(
                    DidChangeTextDocument::METHOD,
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [ { "text": text } ],
                    }),
                )?;
            }
            None => {
                self.docs.insert(uri.to_string(), (text.to_string(), 1));
                self.send_notification(
                    DidOpenTextDocument::METHOD,
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id_for(uri),
                            "version": 1,
                            "text": text,
                        }
                    }),
                )?;
            }
        }
        Ok(())
    }

    /// Pull diagnostics when the server supports `textDocument/diagnostic`
    /// (LSP 3.17); otherwise wait for pushes to settle — a report newer than
    /// the document's last change, then `settle` of quiet, at most
    /// `settle_cap` — and return what was published (FR-LSP-07).
    fn diagnostics(&mut self, uri: Option<&str>) -> Result<Box<[LspDiagnostic]>, BoxError> {
        if let (Some(u), false) = (uri, self.capabilities["diagnosticProvider"].is_null()) {
            let result = self.send_request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": u } }),
            )?;
            if result["kind"] == "full" {
                let items = parse_diagnostics(u, &result["items"]);
                self.diagnostics
                    .insert(u.to_string(), (items, Instant::now()));
            }
            return Ok(self.stored(uri));
        }
        let started = Instant::now();
        let cap = started + self.settle_cap;
        loop {
            let fresh = match uri {
                Some(u) => match (self.diagnostics.get(u), self.changed_at.get(u)) {
                    (Some((_, got)), Some(changed)) => got >= changed,
                    (Some(_), None) => true,
                    (None, _) => false,
                },
                None => true,
            };
            let quiet_since = self.last_push.unwrap_or(started).max(started);
            let quiet = quiet_since.elapsed() >= self.settle;
            if (fresh && quiet) || Instant::now() >= cap {
                break;
            }
            self.pump((Instant::now() + Duration::from_millis(50)).min(cap))?;
        }
        Ok(self.stored(uri))
    }

    fn stored_diagnostics(&self, uri: &str) -> Box<[LspDiagnostic]> {
        self.stored(Some(uri))
    }

    fn workspace_symbols(&mut self, query: &str) -> Result<Box<[LspSymbolInfo]>, BoxError> {
        let result = self.send_request("workspace/symbol", json!({ "query": query }))?;
        Ok(parse_symbols(&result))
    }

    fn diagnostics_within(
        &mut self,
        uri: &str,
        cap: Duration,
    ) -> Result<Box<[LspDiagnostic]>, BoxError> {
        let saved = self.settle_cap;
        self.settle_cap = cap.min(saved);
        let result = self.diagnostics(Some(uri));
        self.settle_cap = saved;
        result
    }

    fn readiness(&self) -> LspReadiness {
        match self.progress.values().next() {
            None => LspReadiness::Ready,
            Some(_) => LspReadiness::Indexing(self.progress.values().flatten().copied().min()),
        }
    }
}

/// `Diagnostic[]` → domain diagnostics. `code` may be a number or a string.
pub fn parse_diagnostics(uri: &str, items: &Value) -> Vec<LspDiagnostic> {
    items
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    Some(LspDiagnostic {
                        uri: uri.to_string(),
                        range: parse_range(&d["range"])?,
                        severity: d["severity"].as_u64().map_or(1, |s| s.clamp(1, 4) as u8),
                        code: match &d["code"] {
                            Value::String(c) => Some(c.clone()),
                            Value::Number(n) => Some(n.to_string()),
                            _ => None,
                        },
                        message: d["message"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `SymbolInformation[]` or `WorkspaceSymbol[]` → domain symbols. A
/// `WorkspaceSymbol` may carry a location with no range; it becomes 0:0.
pub fn parse_symbols(result: &Value) -> Box<[LspSymbolInfo]> {
    result
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let loc = &s["location"];
                    let uri = loc["uri"].as_str()?.to_string();
                    let range = parse_range(&loc["range"]).unwrap_or(LspRange {
                        start: LspPosition {
                            line: 0,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 0,
                            character: 0,
                        },
                    });
                    Some(LspSymbolInfo {
                        name: s["name"].as_str()?.to_string(),
                        kind: s["kind"].as_u64().unwrap_or(0) as u32,
                        container: s["containerName"].as_str().map(str::to_string),
                        location: LspLocation { uri, range },
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Wire framing + response parsing (pure functions, unit-tested without a server)
// ---------------------------------------------------------------------------

/// Read one `Content-Length`-framed JSON message.
fn read_frame<R: Read>(reader: &mut BufReader<R>) -> Result<Value, LspError> {
    let mut content_length: Option<usize> = None;
    // Headers: read byte-wise so a malformed stream cannot over-read into the
    // body of the next message.
    loop {
        let line = read_header_line(reader)?;
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(|r| r.trim().to_string())
        {
            content_length = rest.parse::<usize>().ok();
        }
    }
    let len = content_length.ok_or_else(|| LspError::Protocol("missing Content-Length".into()))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| LspError::Protocol(e.to_string()))
}

fn read_header_line<R: Read>(reader: &mut BufReader<R>) -> Result<String, LspError> {
    let mut line = Vec::with_capacity(32);
    let mut byte = [0u8; 1];
    loop {
        reader.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            line.push(byte[0]);
        }
    }
    String::from_utf8(line).map_err(|e| LspError::Protocol(e.to_string()))
}

fn parse_range(value: &Value) -> Option<LspRange> {
    let start = value.get("start")?;
    let end = value.get("end")?;
    Some(LspRange {
        start: LspPosition {
            line: start.get("line")?.as_u64()? as u32,
            character: start.get("character")?.as_u64()? as u32,
        },
        end: LspPosition {
            line: end.get("line")?.as_u64()? as u32,
            character: end.get("character")?.as_u64()? as u32,
        },
    })
}

fn parse_one_location(value: &Value) -> Option<LspLocation> {
    // `Location { uri, range }` or `LocationLink { targetUri, targetSelectionRange }`.
    if let (Some(uri), Some(range)) = (
        value.get("uri").and_then(|u| u.as_str()),
        value.get("range").and_then(parse_range_opt),
    ) {
        return Some(LspLocation {
            uri: uri.to_string(),
            range,
        });
    }
    let uri = value.get("targetUri").and_then(|u| u.as_str())?;
    let range = value
        .get("targetSelectionRange")
        .and_then(parse_range_opt)
        .or_else(|| value.get("targetRange").and_then(parse_range_opt))?;
    Some(LspLocation {
        uri: uri.to_string(),
        range,
    })
}

fn parse_range_opt(value: &Value) -> Option<LspRange> {
    parse_range(value)
}

/// `textDocument/definition` → the first location, whatever shape it took.
pub fn parse_location(result: &Value) -> Option<LspLocation> {
    match result {
        Value::Array(items) => items.iter().find_map(parse_one_location),
        Value::Null => None,
        other => parse_one_location(other),
    }
}

/// `textDocument/references` → every location.
pub fn parse_locations(result: &Value) -> Box<[LspLocation]> {
    match result {
        Value::Array(items) => items
            .iter()
            .filter_map(parse_one_location)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        Value::Null => Box::new([]),
        other => parse_one_location(other)
            .map(|l| vec![l])
            .unwrap_or_default()
            .into_boxed_slice(),
    }
}

/// `textDocument/hover` → plain text. Handles all three `contents` shapes
/// (string, `MarkedString`, array of either, `MarkupContent`).
pub fn parse_hover(result: &Value) -> String {
    let Some(contents) = result.get("contents") else {
        return String::new();
    };
    fn one(value: &Value) -> Option<String> {
        match value {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => value
                .get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        }
    }
    match contents {
        Value::Array(items) => items
            .iter()
            .filter_map(one)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        other => one(other).unwrap_or_default().trim().to_string(),
    }
}

/// `textDocument/rename` → a flat list of edits. Both the legacy `changes`
/// map and the newer `documentChanges` array are supported. The edits are
/// **advice**: task-16 applies them through the native file tools so there is
/// exactly one write path (FR-LSP-02).
pub fn parse_workspace_edit(result: &Value) -> LspWorkspaceEdit {
    let mut changes: Vec<LspTextEdit> = Vec::new();

    if let Some(map) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in map {
            if let Some(list) = edits.as_array() {
                for edit in list {
                    if let Some(e) = parse_text_edit(uri, edit) {
                        changes.push(e);
                    }
                }
            }
        }
    }

    if let Some(doc_changes) = result.get("documentChanges").and_then(|c| c.as_array()) {
        for doc in doc_changes {
            let Some(uri) = doc
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(|u| u.as_str())
            else {
                continue;
            };
            if let Some(list) = doc.get("edits").and_then(|e| e.as_array()) {
                for edit in list {
                    if let Some(e) = parse_text_edit(uri, edit) {
                        changes.push(e);
                    }
                }
            }
        }
    }

    LspWorkspaceEdit {
        changes: changes.into_boxed_slice(),
    }
}

fn parse_text_edit(uri: &str, edit: &Value) -> Option<LspTextEdit> {
    Some(LspTextEdit {
        uri: uri.to_string(),
        range: edit.get("range").and_then(parse_range_opt)?,
        new_text: edit.get("newText")?.as_str()?.to_string(),
    })
}

/// Map a file extension to an LSP `languageId`. Unknown extensions fall back
/// to `plaintext` rather than failing the `didOpen`.
pub fn language_id_for(uri: &str) -> &'static str {
    let ext = uri.rsplit('.').next().unwrap_or_default();
    match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" => "javascript",
        "jsx" => "javascriptreact",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "sh" | "bash" => "shellscript",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" => "markdown",
        _ => "plaintext",
    }
}

/// `file://` URI for a filesystem path (percent-encoding the few characters
/// that would otherwise break the URI; no `url` dependency needed).
pub fn path_to_uri(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len() + 8);
    out.push_str("file://");
    for ch in raw.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

/// Inverse of [`path_to_uri`] for edits that come back from a server.
pub fn uri_to_path(uri: &str) -> std::path::PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let decoded = raw
        .replace("%20", " ")
        .replace("%23", "#")
        .replace("%3F", "?")
        .replace("%25", "%");
    std::path::PathBuf::from(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_location_array() {
        let result = json!([{
            "uri": "file:///src/main.rs",
            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 9 } }
        }]);
        let loc = parse_location(&result).expect("location");
        assert_eq!(loc.uri, "file:///src/main.rs");
        assert_eq!(loc.range.start.line, 3);
        assert_eq!(loc.range.end.character, 9);
    }

    #[test]
    fn parses_scalar_location_and_location_link() {
        let scalar = json!({
            "uri": "file:///a.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }
        });
        assert_eq!(parse_location(&scalar).unwrap().uri, "file:///a.rs");

        let link = json!([{
            "targetUri": "file:///b.rs",
            "targetRange": { "start": { "line": 1, "character": 0 }, "end": { "line": 5, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 1, "character": 3 }, "end": { "line": 1, "character": 6 } }
        }]);
        let loc = parse_location(&link).expect("link");
        assert_eq!(loc.uri, "file:///b.rs");
        // The selection range (the symbol itself) wins over the full range.
        assert_eq!(loc.range.start.character, 3);
    }

    #[test]
    fn null_definition_is_none_not_panic() {
        assert!(parse_location(&Value::Null).is_none());
        assert!(parse_locations(&Value::Null).is_empty());
    }

    #[test]
    fn parses_references_list() {
        let result = json!([
            { "uri": "file:///a.rs", "range": { "start": { "line": 1, "character": 1 }, "end": { "line": 1, "character": 2 } } },
            { "uri": "file:///b.rs", "range": { "start": { "line": 2, "character": 1 }, "end": { "line": 2, "character": 2 } } }
        ]);
        let locs = parse_locations(&result);
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[1].uri, "file:///b.rs");
    }

    #[test]
    fn parses_hover_shapes() {
        assert_eq!(
            parse_hover(&json!({ "contents": "plain text" })),
            "plain text"
        );
        assert_eq!(
            parse_hover(&json!({ "contents": { "kind": "markdown", "value": "fn foo()" } })),
            "fn foo()"
        );
        assert_eq!(
            parse_hover(&json!({ "contents": [
                { "language": "rust", "value": "fn foo()" },
                "docs here"
            ] })),
            "fn foo()\ndocs here"
        );
        assert_eq!(parse_hover(&json!({})), "");
    }

    #[test]
    fn parses_workspace_edit_changes_map() {
        let result = json!({
            "changes": {
                "file:///src/model.rs": [
                    { "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 7 } },
                      "newText": "bar" }
                ]
            }
        });
        let edit = parse_workspace_edit(&result);
        assert_eq!(edit.changes.len(), 1);
        assert_eq!(edit.changes[0].uri, "file:///src/model.rs");
        assert_eq!(edit.changes[0].new_text, "bar");
        assert_eq!(edit.changes[0].range.start.line, 10);
    }

    #[test]
    fn parses_workspace_edit_document_changes() {
        let result = json!({
            "documentChanges": [{
                "textDocument": { "uri": "file:///src/lib.rs", "version": 2 },
                "edits": [
                    { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } },
                      "newText": "ctx" }
                ]
            }]
        });
        let edit = parse_workspace_edit(&result);
        assert_eq!(edit.changes.len(), 1);
        assert_eq!(edit.changes[0].new_text, "ctx");
    }

    #[test]
    fn language_ids_cover_common_extensions() {
        assert_eq!(language_id_for("file:///a/b.rs"), "rust");
        assert_eq!(language_id_for("file:///a/b.py"), "python");
        assert_eq!(language_id_for("file:///a/b.unknown"), "plaintext");
    }

    #[test]
    fn uri_path_round_trip() {
        let p = std::path::Path::new("/tmp/my project/a.rs");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///tmp/my%20project/a.rs");
        assert_eq!(uri_to_path(&uri), p);
    }

    #[test]
    fn reads_content_length_frame() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = BufReader::new(std::io::Cursor::new(raw.into_bytes()));
        let value = read_frame(&mut reader).expect("frame");
        assert_eq!(value["result"]["ok"], json!(true));
    }

    #[test]
    fn frame_without_content_length_is_protocol_error() {
        let mut reader = BufReader::new(std::io::Cursor::new(b"X-Other: 1\r\n\r\n".to_vec()));
        assert!(matches!(
            read_frame(&mut reader),
            Err(LspError::Protocol(_))
        ));
    }

    #[test]
    fn missing_server_is_error_not_panic() {
        let Err(err) = LspClient::start(
            "/nonexistent/language-server",
            &[],
            &[],
            std::path::Path::new("."),
        ) else {
            panic!("spawning a missing binary must fail");
        };
        assert!(matches!(err, LspError::Spawn(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn handshake_timeout_returns_error_not_hang() {
        let Err(err) = LspClient::start_with_timeout(
            "sh",
            &["-c".into(), "cat > /dev/null".into()],
            &[],
            std::path::Path::new("."),
            300,
        ) else {
            panic!("a silent server must time out, not connect");
        };
        assert!(matches!(err, LspError::Timeout(_)), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn open_document_mirrors_text_and_bumps_version() {
        // A server that only answers `initialize` is enough: didOpen/didChange
        // are notifications, so no reply is expected.
        let script = r#"body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'; printf 'Content-Length: %d\r\n\r\n%s' ${#body} "$body"; cat > /dev/null"#;
        let mut client = LspClient::start_with_timeout(
            "sh",
            &["-c".into(), script.into()],
            &[],
            std::path::Path::new("."),
            5_000,
        )
        .expect("handshake");

        client
            .open_document("file:///a.rs", "fn main() {}")
            .unwrap();
        assert_eq!(client.documents()["file:///a.rs"].1, 1);
        client
            .open_document("file:///a.rs", "fn main() { let x = 1; }")
            .unwrap();
        let (text, version) = &client.documents()["file:///a.rs"];
        assert_eq!(version, &2, "second open must be a didChange");
        assert!(text.contains("let x"));
    }

    /// Live rust-analyzer integration (L3). Needs `rust-analyzer` on PATH.
    #[test]
    #[ignore = "requires rust-analyzer on PATH"]
    fn rust_analyzer_resolves_definition() {
        let root = std::path::Path::new(".");
        let mut client =
            LspClient::start_with_timeout("rust-analyzer", &[], &[], root, 60_000).expect("start");
        let path = root.join("crates/domain/src/model.rs");
        let text = std::fs::read_to_string(&path).expect("read model.rs");
        let uri = path_to_uri(&path.canonicalize().unwrap());
        client.open_document(&uri, &text).unwrap();
        let hover = client.hover(&uri, 6, 12).expect("hover");
        assert!(!hover.is_empty());
    }

    // ---- CE-DQ19: routing, diagnostics, symbols, readiness ----------------

    fn frame(v: serde_json::Value) -> Vec<u8> {
        let body = v.to_string();
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    /// A "server" that plays back `before` at once, then `later` after
    /// `delay_ms`, then stays alive. It never reads stdin, so it is fully
    /// deterministic; the client's replies are simply buffered.
    fn scripted(
        before: Vec<serde_json::Value>,
        later: Vec<serde_json::Value>,
        delay_ms: u64,
    ) -> (tempfile::TempDir, LspClient) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, frames: &[serde_json::Value]| {
            let bytes: Vec<u8> = frames.iter().cloned().flat_map(frame).collect();
            std::fs::write(dir.path().join(name), bytes).unwrap();
        };
        write("before", &before);
        write("later", &later);
        let script = format!(
            "cat '{b}'; sleep {d}; cat '{l}'; sleep 30",
            b = dir.path().join("before").display(),
            l = dir.path().join("later").display(),
            d = delay_ms as f64 / 1000.0
        );
        let client = LspClient::start_with_timeout(
            "sh",
            &["-c".to_string(), script],
            &[],
            dir.path(),
            5_000,
        )
        .expect("scripted server starts");
        (dir, client)
    }

    fn init(capabilities: serde_json::Value) -> serde_json::Value {
        json!({ "jsonrpc": "2.0", "id": 1, "result": { "capabilities": capabilities } })
    }

    fn diag(uri: &str, line: u32, message: &str) -> serde_json::Value {
        json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
            "uri": uri,
            "diagnostics": [{
                "range": { "start": { "line": line, "character": 4 },
                           "end": { "line": line, "character": 9 } },
                "severity": 1, "code": "E0308", "message": message
            }]
        }})
    }

    #[test]
    fn server_requests_are_answered_and_notifications_kept() {
        let (_dir, mut client) = scripted(
            vec![
                init(json!({})),
                json!({ "jsonrpc": "2.0", "id": "srv-1", "method": "workspace/configuration",
                        "params": { "items": [{}, {}] } }),
                diag("file:///a.rs", 3, "mismatched types"),
                json!({ "jsonrpc": "2.0", "method": "$/progress", "params": {
                    "token": "idx", "value": { "kind": "begin", "percentage": 40 } } }),
                json!({ "jsonrpc": "2.0", "id": 2, "result": {
                    "uri": "file:///b.rs",
                    "range": { "start": { "line": 1, "character": 2 },
                               "end": { "line": 1, "character": 5 } } } }),
            ],
            vec![],
            0,
        );
        // The configuration request arrives *before* the definition reply; a
        // client that ignored it (as v0.6 did) could stall a real server.
        let loc = client.goto_definition("file:///a.rs", 0, 0).unwrap();
        assert_eq!(loc.uri, "file:///b.rs");
        let stored = client.stored_diagnostics("file:///a.rs");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].code.as_deref(), Some("E0308"));
        assert_eq!(client.readiness(), LspReadiness::Indexing(Some(40)));
    }

    #[test]
    fn push_diagnostics_wait_for_a_report_newer_than_the_change() {
        let (_dir, mut client) = scripted(
            vec![init(json!({})), diag("file:///a.rs", 0, "stale")],
            vec![diag("file:///a.rs", 7, "fresh after the edit")],
            400,
        );
        client.set_diagnostics_settle(Duration::from_millis(100), Duration::from_secs(4));
        client
            .open_document("file:///a.rs", "fn main() {}")
            .unwrap();
        let started = Instant::now();
        let found = client.diagnostics(Some("file:///a.rs")).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "fresh after the edit");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "settled, did not hit the cap"
        );
    }

    #[test]
    fn pull_diagnostics_are_used_when_the_server_supports_them() {
        let (_dir, mut client) = scripted(
            vec![
                init(json!({ "diagnosticProvider": { "interFileDependencies": false } })),
                json!({ "jsonrpc": "2.0", "id": 2, "result": { "kind": "full", "items": [{
                    "range": { "start": { "line": 2, "character": 0 },
                               "end": { "line": 2, "character": 1 } },
                    "severity": 2, "code": 12, "message": "unused variable" }] } }),
            ],
            vec![],
            0,
        );
        let found = client.diagnostics(Some("file:///c.rs")).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, 2);
        assert_eq!(found[0].code.as_deref(), Some("12"));
    }

    #[test]
    fn workspace_symbols_parse_both_result_shapes() {
        let result = json!([
            { "name": "execute", "kind": 6, "containerName": "AgentLoop",
              "location": { "uri": "file:///app.rs",
                            "range": { "start": { "line": 385, "character": 7 },
                                       "end": { "line": 385, "character": 14 } } } },
            { "name": "App", "kind": 23, "location": { "uri": "file:///lib.rs" } }
        ]);
        let symbols = parse_symbols(&result);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].container.as_deref(), Some("AgentLoop"));
        assert_eq!(symbols[0].location.range.start.line, 385);
        assert_eq!(symbols[1].location.range.start.line, 0);
    }
}
