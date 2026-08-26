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
    BoxError, LspLocation, LspPort, LspPosition, LspRange, LspTextEdit, LspWorkspaceEdit,
};
use lsp_types::notification::{
    DidChangeTextDocument, DidOpenTextDocument, Initialized, Notification,
};
use lsp_types::request::{GotoDefinition, HoverRequest, Initialize, References, Rename, Request};
use serde_json::{json, Value};

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

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
    rx: Receiver<Value>,
    next_id: u64,
    /// uri -> (text, version); the version is bumped on every `didChange`
    /// so the server's view stays in sync with our edits (FR-LSP-04).
    docs: HashMap<String, (String, i32)>,
    timeout: Duration,
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
                if tx.send(value).is_err() {
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
                    "synchronization": { "didSave": false }
                }
            },
            "clientInfo": { "name": "zcode", "version": env!("CARGO_PKG_VERSION") },
        });
        self.send_request(Initialize::METHOD, params)?;
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
            let value = self.read_message(deadline)?;
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
                // Diagnostics, progress notifications, and server->client
                // requests all arrive on the same stream: skip them.
                _ => continue,
            }
        }
    }

    fn read_message(&mut self, deadline: Instant) -> Result<Value, LspError> {
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
}
