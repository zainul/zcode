# Task 30 — LSP: Symbol Addressing, Compact Output, Diagnostics, Guaranteed Document Sync

**Related PRD sections:** §5.2 FR-LSP-05, 06, 07, 09; §1.1 B10; §4.2 principle 6
**Technical plan:** CE-DQ14, CE-DQ19, CE-DQ21, §7.3, §8
**Depends on:** task-25 (`render.rs`), task-27 (`after_write` hook). Symbol addressing uses the code index when present (task-32), so this task implements the `workspace/symbol` path and the index path is a one-line addition in task-33.
**Phase:** 2 (v0.8.0)
**Status:** Done (v0.7.0)
**Priority:** High. Makes the semantic tools cheaper than reading, rather than more expensive.

## Objective

1. The LSP client stops discarding server notifications: it stores diagnostics and progress, and **answers server→client requests** (CE-DQ19, which also fixes a latent stall).
2. The position tools accept `symbol` as well as a 1-based `line`/`column`.
3. Output is grouped, compact and capped.
4. A new read-only `lsp__diagnostics` tool.
5. Every zcode write syncs the document to the server before the next LSP request.

## Step-by-step

### 1. `infra-lsp`: message routing (CE-DQ19)

In `LspClient::send_request`'s read loop, replace `_ => continue` with a `route(value)` call:

```rust
fn route(&mut self, v: &Value) -> Result<(), LspError> {
    match (v.get("id"), v.get("method").and_then(Value::as_str)) {
        (Some(id), Some(method)) => self.reply_to_server_request(id.clone(), method, v.get("params")),
        (None, Some("textDocument/publishDiagnostics")) => { self.store_diagnostics(v.get("params")); Ok(()) }
        (None, Some("$/progress")) => { self.track_progress(v.get("params")); Ok(()) }
        _ => Ok(()),
    }
}
fn reply_to_server_request(&mut self, id: Value, method: &str, params: Option<&Value>) -> Result<(), LspError> {
    let result = match method {
        "workspace/configuration" => json!(vec![Value::Null; params.and_then(|p| p["items"].as_array()).map_or(0, |a| a.len())]),
        "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" => Value::Null,
        "workspace/applyEdit" => json!({ "applied": false }),   // zcode applies edits itself (task-35)
        _ => return self.write_message(&json!({ "jsonrpc": "2.0", "id": id,
                 "error": { "code": -32601, "message": "method not supported by zcode" } })),
    };
    self.write_message(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}
```

State: `diagnostics: HashMap<String /*uri*/, DiagEntry { version: Option<i32>, items: Vec<Diag>, received: Instant }>`, `progress: HashMap<String, (Option<u8> /*pct*/, bool /*done*/)>`, `capabilities: Value` (kept from the `initialize` response).

`domain` types: `LspDiagnostic { uri, line, character, severity: u8, code: Option<String>, message: String }` (0-based, wire-level).

### 2. `infra-lsp`: new operations

- `fn diagnostics(&mut self, uri: Option<&str>) -> Result<Box<[LspDiagnostic]>, BoxError>`:
  - If `capabilities.diagnosticProvider` is present and `uri` is `Some`: send `textDocument/diagnostic` (pull) → parse `items` (`kind: "full"`), or reuse the stored ones when the result is `unchanged`.
  - Otherwise (push): drain incoming messages until **1.5 s** have passed with no new `publishDiagnostics` for the target (or any, if `uri` is `None`), capped at **10 s**. Then return the stored items. For documents opened this session, wait until an entry newer than the last `didChange` exists (stale diagnostics are worse than none).
- `fn workspace_symbols(&mut self, query: &str) -> Result<Box<[LspSymbolInfo]>, BoxError>` (`workspace/symbol`).
- `fn readiness(&self) -> Readiness { Ready | Indexing(Option<u8>) }` from the `progress` map (any token not done → Indexing).

Add these to `domain::LspPort` **with default bodies** (empty / `Ready`), so existing fakes compile unchanged.

### 3. `tools`: position conversion (CE-DQ14)

`tools/src/lsp_pos.rs` (new):

```rust
pub fn to_wire(line_1: u32, col_1: u32) -> (u32, u32)   // saturating -1
pub fn to_model(line_0: u32, col_0: u32) -> (u32, u32)  // +1
pub enum Target { Position { path: String, line: u32, column: u32 }, Symbol { symbol: String, path: Option<String> } }
pub fn parse_target(args: &Value) -> Result<Target, ToolResult>;
```

**Breaking change for the model-facing schema:** `line` and `column` are now 1-based (were 0-based `line`/`character`). `character` is still accepted as an alias for `column`, but it is interpreted as **1-based** too. That matches every other zcode tool, and the old 0-based convention was the inconsistent one. CHANGELOG *Changed*.

### 4. `tools`: symbol resolution (CE-DQ21)

```rust
fn resolve(&mut self, t: Target) -> Result<(String /*uri*/, u32, u32 /*wire*/), ToolResult> {
    match t {
        Target::Position{..} => …,
        Target::Symbol{symbol, path} => {
            // 1. code index (task-33 adds this branch; absent → skip)
            // 2. lsp.workspace_symbols(last segment of symbol) filtered by container/qualified match and path prefix
            // 3. 0 → error "no symbol named X"; >1 → error listing ≤10 candidates "kind name — path:line"
        }
    }
}
```

The resolved position is the symbol's **selection range start** (its name identifier).

### 5. Compact rendering (FR-LSP-06)

Through `render.rs`:

- `goto_definition`: `src/lib.rs:120:8  pub fn execute(&mut self, …)`. The line text is read from disk (the LSP result has no text), clipped to 160 chars.
- `find_references`: grouped by file, `  120:8  let x = foo();`, ordered by path then line, capped at 50, with the `Footer` (total, and "narrow with path").
- `hover`: strip ```` ``` ```` fences and language tags, collapse blank-line runs, drop a signature line that repeats the previous line, cap at 1,500 chars.
- `rename_symbol` (advice mode, unchanged semantics): `would edit 14 locations in 6 files` + a grouped list capped at 30.

### 6. `lsp__diagnostics` tool (FR-LSP-07)

Read-only; not added to any deny list. Params: `path` (optional), `severity` (`error` default | `warning` | `all`). Output: `path:line:col  error[E0308]  mismatched types …` (1-based, message first line only, clipped to 200 chars), grouped and sorted by path/line, capped at 50, with the footer. With `path` omitted: every document opened or changed in this session. `subject: Diagnostics { path }` (for task-28 supersession).

### 7. Guaranteed sync (FR-LSP-09)

Register an `after_write` listener (task-27 hook) in `ToolRegistry::from_config` when LSP is present: `lsp.open_document(path_to_uri(abs), new_text)`. The existing `open_document` already sends `didOpen` or `didChange` with a bumped version (see the `open_document_mirrors_text_and_bumps_version` test). Also call `didSave`-less sync when `read` opens a file the server has not seen (FR-LSP-04 original intent), so that references resolve in files the model has read.

### 8. Descriptions

One compact schema per tool. `path` + `symbol` are documented in the shared `position_schema` description: *"Address by symbol (e.g. \"AgentLoop::execute\") or by path+line+column (1-based)."*

## Tests

- `server_configuration_request_is_answered` (a fake server script, using the existing canned-stdio approach, sends `workspace/configuration` before replying to `definition`; the client must not hang).
- `publish_diagnostics_are_stored_by_uri`; `progress_tracks_indexing`.
- `pull_diagnostics_used_when_capable` (canned).
- `push_diagnostics_wait_for_post_change_version`.
- `positions_are_one_based_at_the_tool_boundary`; `character_alias_is_one_based`.
- `symbol_target_resolves_via_workspace_symbol`; `ambiguous_symbol_lists_candidates`; `unknown_symbol_is_model_error`.
- Rendering snapshots: definition, references (capped with footer), hover (fence stripping), diagnostics.
- `every_write_syncs_the_document` (fake `LspPort` records the `open_document` calls after `str_replace`, `write`, `apply_patch`).
- Line-number contract (extends task-26): `grep` line N → `lsp__hover line=N` hovers the same identifier (fake server echoes the position).
- Integration `#[ignore]` (rust-analyzer): `diagnostics_report_an_injected_type_error`; `find_references_by_symbol_name`.

## Test-case scenario

Planning mode: "what calls `truncate_tool_output`?" → `lsp__find_references symbol="truncate_tool_output"` → 4 references grouped by file, in about 150 tokens, with no read. After an edit that breaks a type, `lsp__diagnostics path=crates/app/src/lib.rs` → one `error[E0308]` line instead of a 6 k-token `cargo check` log.

## How to verify

```sh
cargo test -p infra-lsp
cargo test -p tools lsp
cargo test -p infra-lsp -- --ignored      # needs rust-analyzer on PATH
make ci
```

**Pass criteria:** tests green; no LSP call blocks on a server→client request; the evaluation corpus shows LSP tool use rising and `verify`-category tokens below the shell build logs they replace (report `by_category`).

## Success metric mapping

M3, GR2, GR3. FR-LSP-05, 06, 07, 09. Resolves B10.
