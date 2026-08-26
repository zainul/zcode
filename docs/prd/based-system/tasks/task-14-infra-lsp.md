# Task 14 — Infra: LSP Client + LspPort

**Related PRD sections:** §3.3.3 LSP Integration (FR-LSP-01..04), §3.3 Extensible Tool System, §8 DQ7 (LSP client library)
**Depends on:** task-02 (Domain — `LspPort` trait defined in §4.3 of technical plan)
**Status:** Done
**Priority:** Medium (semantic code intel; graceful if a server is absent)

## Objective

Implement a stdio JSON-RPC LSP client (`LspClient`) in `crates/infra/lsp` satisfying `domain::LspPort` with `goto_definition`, `find_references`, `hover`, `rename_symbol`, and `open_document`. The client keeps an in-memory document-state map and pushes `textDocument/didOpen`/`didChange` so edits stay in sync (FR-LSP-04). Uses `lsp-types` for wire types but exposes only domain-owned types across the port boundary (no `lsp-types` leakage into `domain`).

## Step-by-step

### 1. New crate `crates/infra/lsp`

`Cargo.toml`:
```toml
[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
lsp-types = "0.97"
serde_json = { workspace = true }
```

### 2. `src/lib.rs` — `LspClient`

```rust
pub struct LspClient {
    child: std::process::Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
    next_id: u64,
    docs: HashMap<String /* uri */, String /* text */>,
}
impl LspClient {
    pub fn start(command: &str, args: &[String], env: &[(String,String)]) -> Result<Self, LspError>;
    fn send_request(&mut self, method: &str, params: serde_json::Value) -> Result<Option<serde_json::Value>, LspError>;
    fn send_notification(&mut self, method: &str, params: serde_json::Value) -> Result<(), LspError>;
    fn read_message(&mut self, deadline_ms: u64) -> Result<serde_json::Value, LspError>;
}
impl LspPort for LspClient {
    fn open_document(&mut self, uri: &str, text: &str) -> Result<(), Box<dyn Error>> {
        self.docs.insert(uri.into(), text.into());
        self.send_notification("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": lang_id(uri), "version": 1, "text": text } }))
    }
    fn goto_definition(&mut self, uri:&str, line:u32, character:u32) -> Result<LspLocation, Box<dyn Error>>;
    fn find_references(&mut self, uri:&str, line:u32, character:u32) -> Result<Box<[LspLocation]>, Box<dyn Error>>;
    fn hover(&mut self, uri:&str, line:u32, character:u32) -> Result<String, Box<dyn Error>>;
    fn rename_symbol(&mut self, uri:&str, line:u32, character:u32, new_name:&str) -> Result<LspWorkspaceEdit, Box<dyn Error>>;
}
```

### 3. Wire protocol

- `initialize` → `initialized` notification → `textDocument/didOpen` on first access (FR-LSP-04).
- `goto_definition` → `textDocument/definition`; map `lsp-types::GotoDefinitionResponse::Array` of `Location` → domain `LspLocation { uri, range }`. (`LspLocation`/`LspWorkspaceEdit` are domain types defined in §4.3.)
- `find_references` → `textDocument/references` with `includeDeclaration=false`.
- `hover` → `textDocument/hover`; join `contents` to a Markdown-free string.
- `rename_symbol` → `textDocument/rename`; extract `DocumentChanges` edits → `LspWorkspaceEdit` (a list of `{uri, range, new_text}` applied later by task-16 via the native `str_replace` tool — **the LSP result is advice, the actual edit goes through the file tool**, satisfying FR-LSP-02 without duplicating file writes).

### 4. Document sync on edits

When task-15's `str_replace` tool edits a file, the engine should also call `LspPort::open_document` (or `didChange`) with the new text so `find_references`/`rename` stay accurate (FR-LSP-04). The `ToolRegistry` is given an optional `LspPort` handle by `wire()` for this.

### 5. Tests

- `parse_goto_definition`: feed a canned `textDocument/definition` response (via a fake echo command piping JSON) → assert `LspLocation` fields.
- `parse_hover`: canned hover `contents` → joined string.
- `parse_rename_workspace_edit`: canned `WorkspaceEdit` → `LspWorkspaceEdit` with the right uri+range+new_text.
- `open_document_stores_text`: `open_document` then internal `docs` map has the text (access a `pub(crate)`/`pub` field or a test-only `fn docs(&self)`).
- `missing_server_skipped`: `start()` on a nonexistent command → `Err`; `wire()` records it as absent (no crash).

Integration (subprocess, `#[ignore]`):
- `rust_analyzer_resolves_def`: `#[ignore]`; spin up `rust-analyzer` in a temp rust project, `goto_definition` resolves a known symbol (L3).

## Test-case scenario

- `zcode run "show me callers of foo"` in a Rust crate → the LSP-backed `find_references` tool resolves real references via rust-analyzer; results are fed back to the LLM.

## How to verify

```
cargo test -p infra-lsp
cargo test -p infra-lsp -- --ignored            # needs rust-analyzer on PATH
cargo clippy -p infra-lsp -- -D warnings
cargo tree -p infra-lsp                       # deps: domain, lsp-types, serde_json
```

**Pass criteria:** JSON-RPC request/response round-trips parse correctly into **domain-owned** types; `open_document` keeps local state; a missing server is an `Err`, never a panic; zero `unsafe`; `cargo tree -p infra-lsp` shows `{domain, lsp-types, serde_json}`.

## Success metric mapping

- M1.2/M1.3, FR-LSP-01..04, DQ7 (`lsp-types` + hand-rolled JSON-RPC, no `tower-lsp` server crate), NFR-REL-04 (child dropped → killed). L3 = rust-analyzer resolves (integration).
