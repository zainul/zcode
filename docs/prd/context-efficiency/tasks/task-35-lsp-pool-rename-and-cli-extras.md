# Task 35 — LSP Pool (Multi-Server, Lazy, Idle Shutdown), Honest Readiness, Rename `apply`, Diagnostics-on-Edit; `/compact`, `zcode ignore check`

**Related PRD sections:** §5.2 FR-LSP-08, 10, 11, 12; §5.3 FR-CTX-12; §5.5 FR-FILTER-07; §12 R7
**Technical plan:** CE-DQ19, CE-DQ20, CE-DQ21, §7.3, §9
**Depends on:** task-30 (routing, diagnostics, readiness), task-33 (index fallback for readiness), task-34 (`edit_symbol` result hook), task-29 (manual compaction entry point), task-24 (`DiscoveryFilter::explain`)
**Phase:** 3 (v0.9.0)
**Status:** Done (v0.7.0)
**Priority:** Medium. Correctness and comfort features that complete the milestone.

## Objective

1. Replace "the first LSP server that starts wins" with `LspPool`: one slot per language, started lazily on first use, capped, and shut down when idle (CE-DQ20).
2. While a server is indexing, answer from the code index (labelled) instead of blocking until the timeout (FR-LSP-11).
3. `lsp__rename_symbol apply: true`: an atomic multi-file rename applied in-process (FR-LSP-12).
4. Append the *new* errors after every edit when a server covers the file (FR-LSP-08).
5. `/compact [focus]` and `zcode session compact` (FR-CTX-12); `zcode ignore check <path>` (FR-FILTER-07).

## Step-by-step

### 1. `infra-lsp/src/pool.rs` (CE-DQ20)

```rust
pub struct ServerConfig { pub language: String, pub command: String, pub args: Vec<String>, pub env: Vec<(String,String)> }
pub struct LspPool { root: PathBuf, timeout: Duration, max_servers: usize, idle: Duration,
                     slots: Vec<Slot> }
struct Slot { cfg: ServerConfig, client: Option<LspClient>, last_used: Instant, failed: Option<String> }

impl LspPool {
    pub fn new(root, servers: Vec<ServerConfig>, max_servers, idle, timeout) -> Self;   // starts nothing
    fn slot_for(&mut self, uri: &str) -> Result<&mut LspClient, BoxError>;
}
impl LspPort for LspPool { /* every method: evict_idle(); slot_for(uri)?.method(...) */ }
```

- `slot_for`: `language_id_for(uri)` → `canonical_language` (the existing `infra-config` mapping) → the matching slot. A slot with no client: if running clients ≥ `max_servers`, shut down the least-recently-used one (`LspClient::drop` already sends shutdown/exit), then `LspClient::start_with_timeout`. A start failure → `slot.failed = Some(msg)` (not retried for this process; the model gets a model-visible error naming the server and the `zcode config` hint).
- `evict_idle`: clients with `last_used.elapsed() > idle` (default 600 s) are dropped.
- The documents opened through `open_document` are tracked per slot. Re-opening happens automatically after an eviction, because `open_document` is called again on the next write or read.
- Methods that take no URI (`workspace_symbols(query)`, and `diagnostics(None)`) fan out to the **running** clients only. They never start one.
- `readiness(uri)` per slot.

`tools/src/lib.rs` `from_config`: replace the "first server wins" loop with `LspPool::new(root, cfg.effective_lsp_servers() …, cfg.lsp.max_servers, cfg.lsp.idle_shutdown_s)`. `effective_lsp_servers()` keeps its current filtering (it only keeps servers whose binary is on PATH). Detection by project language now only **orders** slots; it no longer limits them to one language (a monorepo needs several).

Config: `[lsp] max_servers = 3`, `idle_shutdown_s = 600`.

### 2. Honest readiness (FR-LSP-11)

In the `lsp__*` tools: before a request, `readiness(uri)`:

- `Indexing(pct)` and the tool is `goto_definition` / `find_references` / `hover`: if the code index can answer (definition → `locate`; references → `related`'s approximate list; hover → the signature + doc comment text from the def span), return that, prefixed `(from code index — language server still indexing: 43%)`. Otherwise return a model-visible error: `language server is still indexing (43%); try grep/symbols, or retry shortly`.
- Never wait out the full request timeout on an indexing server. The request is sent only when `Ready`, or when the server exposes no progress at all.

### 3. Rename `apply` (FR-LSP-12)

- The params gain `apply` (bool, default false). Still gated as a write tool in `planning` (the name is already in `write_tool_names`). With `apply: false` the behaviour is unchanged (advice).
- `apply: true`:
  1. `rename_symbol` → `LspWorkspaceEdit` (already parsed, both the `changes` and `documentChanges` shapes).
  2. For each file: read it, and apply its text edits in **reverse** position order (convert LSP UTF-16 `character` offsets to byte offsets. **Note:** `infra-lsp` currently passes `character` through as a raw number, with no conversion in either direction, which is harmless while edits are advice only. Add `utf16_col_to_byte(line_text, col)` and `byte_to_utf16_col` in `tools/src/lsp_pos.rs`, and use them here and in task-30's 1-based conversion, so columns are right on lines with non-ASCII text. Test with a non-BMP character).
  3. Stage every new content in memory, and write each to `<file>.zcode-tmp`. Then rename all of them. If any stage or tmp write fails → delete the tmp files, nothing renamed, model-visible error (NFR-CTX-REL-04). A rename failure midway is reported with the list of files already renamed (rename is atomic per file; cross-file atomicity is best-effort after the staging phase has proven every write possible).
  4. `after_write` for each file (index + LSP sync).
  5. Result: `renamed foo → bar: 14 occurrences in 6 files` + a per-file count list (no content).

### 4. Diagnostics on edit (FR-LSP-08)

A registry-level post-edit step for `str_replace_editor`, `edit_symbol`, `apply_patch`, `write`, and rename `apply`. When `lsp.diagnostics_on_edit = true` and a *running* client covers the file:

```
before = pool.stored_diagnostics(uri)              // cheap: already cached, no wait
after_write(...)                                   // didChange
after  = pool.diagnostics(Some(uri))               // settle-wait (task-30 rules), max 3s here
new    = errors in `after` whose (message, code) are not in `before`, matched per line window ±3
if !new.is_empty(): append "\nnew errors:\n" + up to 10 rendered lines (1-based)
```

It never starts a server just for this (so an edit to a file in a language with no running server costs nothing extra). With a 3 s cap, a slow server only delays the edit result by the cap.

### 5. `/compact` and `zcode session compact` (FR-CTX-12)

- `SlashCommand::Compact(Option<String>)`: "compact the conversation now [focus text]".
- The engine thread's `Command::Compact(focus)` → `App::compact_now(session_id, focus) -> Result<CompactionRecord, AppError>`. That loads the session, runs `ContextManager::force_compact` with the focus (all tiers, Tier 3 included), archives, and checkpoints. It emits the same `Compacted` event, so the TUI note is identical to automatic compaction.
- `zcode session compact <id> [--focus <text>]`: same, headless, printing `compacted 142k → 38k (summary of steps 3–41)`.
- Refused while a turn is running (`/compact` during a turn → note: "wait for the turn to finish or /stop").

### 6. `zcode ignore check <path>` (FR-FILTER-07)

Prints `excluded by <rule> (<source>)`, e.g. `excluded by default rule node_modules/ (built-in)`, `excluded by "build/" (.gitignore:3)`, `excluded by context.exclude "vendor/**" (~/.config/zcode/config.toml)`. Otherwise it prints `included`. Exit code 0 in both cases (it is an inspection command). Uses `DiscoveryFilter::explain` (task-24), extended to carry the source file and line from `ignore::gitignore::Glob::from()` where available.

## Tests

- Pool: `starts_server_lazily_on_first_use_per_language`; `evicts_lru_when_over_max`; `idle_servers_shut_down`; `failed_start_is_remembered_and_reported`; `uri_less_queries_do_not_start_servers`. Uses a fake server binary built as a test helper: a tiny Rust `[[bin]]` in `infra-lsp` under `cfg(test)`-only examples, or the existing canned-stdio approach.
- Readiness: `indexing_server_answers_from_code_index_with_label`; `indexing_without_index_is_model_error`; `no_progress_capability_means_ready`.
- Rename: `apply_rewrites_all_files_atomically`; `staging_failure_changes_nothing`; `utf16_offsets_convert_correctly` (a line containing `𝒳`); `apply_denied_in_planning`.
- Diagnostics on edit: `new_errors_appended_after_edit`; `no_server_no_wait`; `existing_errors_not_repeated`.
- `/compact`: `slash_compact_parses_focus`; `compact_now_runs_all_tiers_with_focus` (fake summariser sees the focus); `compact_refused_during_turn`.
- `ignore_check_reports_rule_and_source`.
- Integration `#[ignore]`: a Go + TS monorepo fixture with `gopls` and `typescript-language-server` on PATH: each query routes correctly, and TS starts only on first TS use.

## Test-case scenario

A monorepo with `api/` (Go) and `web/` (Next.js). The model edits `web/app/page.tsx` with `edit_symbol`; the TS server starts on the first TS request, and the edit result lists `new errors: web/app/page.tsx:42:7 error TS2322 …`. The model fixes it in one more call, without running `tsc`. Later `lsp__rename_symbol symbol="UserDTO" apply=true` in `api/` renames it across 6 Go files in one call.

## How to verify

```sh
cargo test -p infra-lsp pool
cargo test -p tools rename readiness diagnostics_on_edit
cargo test -p zcode compact ignore_check
cargo test -p infra-lsp -- --ignored monorepo
make ci
make eval-tokens LIVE=1 LABEL=p3-final
```

**Pass criteria:** tests green; **the Phase 3 exit gate: every PRD §2.3 primary metric met and every guardrail held** on the final evaluation run, recorded in `docs/prd/context-efficiency/code-review.md`; peak RSS with 3 servers is reported in the PR (PRD R7).

## Success metric mapping

M1–M7 (final gate), GR2, GR3. FR-LSP-08, 10, 11, 12; FR-CTX-12; FR-FILTER-07.
