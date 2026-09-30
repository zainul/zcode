# Technical Plan: Context Efficiency — zcode v0.7.0 → v0.9.0

**Plan ID:** TP-CTX-EFF-003
**Derived from:** `docs/prd/context-efficiency/prd.md` (PRD-CTX-EFF-003)
**Baseline:** zcode v0.6.0 (`93e0165`)
**Lead Engineer:** Backend Team
**Status:** Draft (ready to implement in the order of §14)

Decisions in this plan are cited in source comments as **`CE-DQn`**. The prefix
keeps them distinct from the `DQn` ids of the earlier milestones, which are still
cited throughout the code.

---

## 1. Executive Summary

The PRD defines *what* has to change to stop zcode wasting context. This plan
defines *how*, file by file, without breaking any architectural rule in CLAUDE.md:

- **Two new infra crates.** `infra-search` embeds ripgrep's engine (`ignore`,
  `globset`, `grep-searcher`, `grep-regex`) behind a `domain::SearchPort`, and owns
  the single `DiscoveryFilter`. `infra-index` embeds tree-sitter behind a
  `domain::CodeIndexPort`, and serves outline, symbol lookup, the file graph, the
  repo map, and the syntax checks that `edit_symbol` relies on.
- **One new app component.** `app::context::ContextManager` runs the three
  compaction tiers between provider calls. Its *policy* (what to supersede, what to
  elide, what is protected) is pure `domain::context` code with property tests.
- **Metadata on messages.** `LlmMessage` gets a `meta` field that request builders
  never serialise. That is how the engine knows which file a tool result is
  about, without `domain` parsing JSON.
- **Cache-correct request builders.** In `infra-llm`, the Anthropic layout uses
  *two* rolling breakpoints (the current request's end *and* the previous
  request's end), so the 20-block lookback can never miss. Usage is split into
  read and write.
- **Two new small ports** so `app` stays I/O-free: `SpillPort` (full tool output
  on disk) and an `archive` method on `SessionStorePort` (messages replaced by
  compaction).

The one fact this plan establishes *by measurement* rather than assumption
(§3): **the newest `ignore` does not compile on the pinned Rust 1.85**, and
tree-sitter plus five grammars adds ~4.8 MB to a release binary. Both shape the
decisions below.

---

## 2. Resolved Decisions

| # | Decision | Resolution | Rationale |
|---|----------|-----------|-----------|
| CE-DQ1 | **Dependency pins and MSRV** | Add `.cargo/config.toml` → `[resolver] incompatible-rust-versions = "fallback"` (stable since Cargo 1.84), so crates that *declare* a higher `rust-version` resolve to older releases. Crates that do **not** declare one but still need a newer compiler are pinned with `=`: **`ignore = "=0.4.29"`** (0.4.30 uses let-chains, a compile error on 1.85; measured, §3). `tree-sitter = "0.26"` (0.27 declares 1.90; the fallback picks 0.26.13), and `tree-sitter-language` resolves to 0.1.7. | Measured, not assumed (PRD Q4). The pinned toolchain in `rust-toolchain.toml` makes `make ci` the MSRV gate, so no separate check is needed. |
| CE-DQ2 | **Port shape for shared services** | `SearchPort` and `CodeIndexPort` take `&self` and require `Send + Sync`. Their implementations hold state behind `RwLock`/`Mutex`. Tools receive `Arc<dyn CodeIndexPort + Send + Sync>` / `Arc<dyn SearchPort + Send + Sync>`, so one index serves `outline`, `symbols`, `related`, `edit_symbol`, the `read` large-file guard, and LSP symbol addressing. | The existing `Tool` trait is `&mut self` per tool. A shared, read-mostly service must not be copied per tool, nor locked for the length of a whole tool call. |
| CE-DQ3 | **Message metadata** | Add `pub meta: MessageMeta` (with `Default`) to `domain::LlmMessage`: `step: u32`, `subject: Option<Subject>`, `tokens_est: u32`, `spill: Option<String>`, `kind: MessageKind { Normal, Summary, Elided, RepoMap }`. `ToolResult` gets `subject: Option<Subject>`, set by the tool that knows it (for example `read` → `Subject::FileRange{path, start, end, hash}`). **Request builders never read `meta`.** The session mirror persists it as optional fields. | Supersession has to know "this result is `src/x.rs` lines 1–400". Only the tool knows that without parsing its arguments, and `domain` cannot parse JSON (FR-DI-01). Keeping `meta` out of the wire keeps request bytes stable (FR-CACHE-02). |
| CE-DQ4 | **Where compaction lives** | Policy: `domain::context` (pure; supersession rules, elision selection, protected set, stub text, step-boundary cuts). Orchestration: `app::context::ContextManager` (trigger, hysteresis, tier order, summariser call, failure fallback, events). Persistence: through ports only (`SessionStorePort::archive`, `SpillPort`). | Satisfies FR-DI-02 (app: domain + thiserror). The policy gets exhaustive property tests with no fakes. |
| CE-DQ5 | **Summariser transport** | `ContextManager` calls `LlmPort::stream` and collects the text. `stream` is the path every adapter implements and tests end to end, whereas `send` is less exercised. An optional second client `App::set_compaction_llm(Box<dyn LlmPort + Send>)` is wired by `cli` when `context.compaction_model` is set. Otherwise the main client is used. | Reuses the retry and decoding path (`send_with_retry`, `SseDecode`) instead of a second code path. |
| CE-DQ6 | **Anthropic breakpoint layout** | BP1: last tool. BP2: the system block. **BP3: the last block of the last message. BP4: the last block of the message that ended the *previous* request**, i.e. the message immediately before the most recent assistant message, when it differs from BP3. The builder can compute BP4 statelessly from the message list, because each step appends exactly `[assistant, tool_results…]`. | A breakpoint exactly where the previous request wrote one is a guaranteed read. This removes the 20-position lookback failure for every step shape, with no state kept in the adapter. Within the limit of 4. |
| CE-DQ7 | **TTL** | `cache.ttl`: `5m` → `{"type":"ephemeral"}`; `1h` → `{"type":"ephemeral","ttl":"1h"}`; `auto` → 1 h on BP1/BP2 for the TUI, 5 m everywhere else (longer TTLs come first, as the API requires). If a gateway returns a 400 that names `ttl`, the adapter falls back to 5 m for the rest of the process and emits one `UiEvent::Notice`. | Uses the same "learn from the rejection" pattern as `WindowTable::learn`. |
| CE-DQ8 | **Marker policy on OpenAI-shaped routes** | Replace the manual `with_cache_control(bool)` flag with `CacheMarkers::{Auto, On, Off}`. `Auto` puts markers on the system message and the last message **iff** the normalised model id starts with `claude`, `anthropic/`, or `gemini`/`google/`. `prompt_cache_key = session_id` is sent **only** when the provider kind is `OpenAi` (other compatible servers may reject unknown fields). | Removes a setting that was wrong by default on new routes. |
| CE-DQ9 | **Split usage** | `LlmFinish { cache_read_tokens, cache_write_tokens }` replace `cache_tokens`. `TelemetryEvent` and `TelemetryTotals` do the same. `PriceEntry` gets `cache_write_multiplier_5m: f64 = 1.25` and `cache_write_multiplier_1h: f64 = 2.0` (the existing `cache_per_mtok` stays the *read* rate). A `cache_tokens()` helper returns `read + write` for code that only needs the total. | FR-CACHE-04/05. The helper keeps the diff small in the TUI and the emitters. |
| CE-DQ10 | **Context arithmetic** | `domain::tokens::PromptSize::from_finish(&LlmFinish, cache_within_input: bool)` returns the true prompt size (Anthropic: `input + read + write`; OpenAI-family: `input`, which already includes cached tokens). The engine keeps `last_reported_prompt` and `appended_since` (a calibrated estimate). | FR-BUDGET-03, FR-CACHE-06. Uses the existing `cache_within_input` knowledge from `domain::pricing`. |
| CE-DQ11 | **Token estimator** | `estimate_tokens(text)`: `ceil(chars / d)` where `d = 3.6` when more than 12 % of chars are ASCII punctuation or symbols (code) and `4.2` otherwise. `TokenCalibrator { ratio }` is an EMA (α = 0.3) of `reported / estimated`, clamped to [0.5, 2.0]. It lives in `domain::tokens` and is stdlib only. | FR-BUDGET-02. A whitespace split undercounts code badly. |
| CE-DQ12 | **Spill and archive storage** | `domain::SpillPort { fn spill(&mut self, session: &str, call_id: &str, content: &str) -> Result<String, BoxError> }` returns a path relative to the working dir. It is implemented by `infra_filesystem::SpillStore` under `.zcode/spill/<session>/<call-id>.txt`, and prunes directories older than `spill_ttl_days` at construction. `SessionStorePort::archive(&mut self, id, messages: &[LlmMessage])` appends JSONL to `sessions/<id>.archive.jsonl`. Both are optional on `App` (`None` → no spill: truncate as before, and the marker says so). | Keeps `app` I/O-free. Removing either store never breaks a run. |
| CE-DQ13 | **Output rendering conventions** | New module `tools::render` owns the shared helpers: `Footer` (totals, next offset, narrowing hint), `clip_line(line, max, centre_col)`, `gutter(n, width)`, `group_by_file`. Every new or changed tool uses them. | One place enforces principles 2 and 3 (bounded, deterministic) and the 1-based line convention (CE-DQ14). |
| CE-DQ14 | **Line numbers** | Model-facing: 1-based everywhere. `domain::LspPort` stays 0-based (it is a wire-level port). Conversion happens *only* in `tools` (`lsp_position_from_model`, `lsp_position_to_model`). | PRD D-3. One conversion point, with a test that round-trips a `grep` hit through `read` and `lsp__hover`. |
| CE-DQ15 | **Tree-sitter queries** | zcode ships its **own** `queries/<lang>/defs.scm` (embedded with `include_str!`) and does not use the grammar crates' `tags.scm`. Qualified names come from walking ancestors (`impl_item`/`trait_item`/`mod_item` for Rust, `class_declaration`/`interface_declaration`/`namespace` for TS, `class_definition` for Python, receiver type for Go methods). | The bundled tags queries differ in coverage and capture names between grammars, and do not produce `Parent::child` names. `edit_symbol` needs exact spans including attached comments and attributes, so zcode must control the captures. |
| CE-DQ16 | **Grammar features** | `infra-index` features `lang-rust`, `lang-go`, `lang-typescript` (TS and TSX), `lang-python`. All four are on by default. The `tools` and `cli` crates forward a `code-index` feature. A build with `--no-default-features` has no tree-sitter at all, and the tools fall back to FR-INDEX-09. | GR5 / NFR-CTX-SIZE-01. The measured ~4.8 MB (§3) can be trimmed per language by a packager. |
| CE-DQ17 | **Index persistence format** (PRD Q1) | A custom format in `infra-index/src/store.rs`: magic `ZCIX`, `u16` format version, then per file a length-prefixed record (path, mtime ns, size, `u64` FNV-1a hash, defs, imports, occurrences), with strings interned in a trailing table. Written atomically (temp file + rename). Any parse error → discard and rebuild. No new dependency. | `serde_json` for ~10 k files × hundreds of occurrences is slow to parse and large. `bincode`/`postcard` would add a dependency for a format we fully control. |
| CE-DQ18 | **Repo-map ranking** | Graph: an edge from file A to file B, weighted `1 / ln(2 + defs_named(n))`, for each identifier `n` that A uses and B defines (so common names count less). Personalised PageRank: damping 0.85, 20 iterations; the teleport vector is weighted towards files whose path or defined names appear in the first user prompt. Render: files in rank order, each with its top-level signatures, until `repo_map_tokens` (estimated with CE-DQ11) is used up. The output is sorted and byte-deterministic. | FR-INDEX-08. Proven ranking in the style of Aider's repo map, sized to be cheap. |
| CE-DQ19 | **LSP notification handling** | `LspClient::send_request` stops *discarding* everything that is not its response. It routes `textDocument/publishDiagnostics` → `diagnostics: HashMap<uri, DiagEntry{version, items, received}>`, `$/progress` → `progress: HashMap<token, Progress>`, and **answers server→client requests** (`workspace/configuration` → array of `null`s, `window/workDoneProgress/create` → `null`, `client/registerCapability` → `null`). | Needed for FR-LSP-07/11. It also fixes a latent bug: a server that waits for a reply to `workspace/configuration` can stall today. |
| CE-DQ20 | **Multi-server LSP** | `infra_lsp::LspPool` implements `LspPort`. It holds `Vec<ServerSlot{language, config, client: Option<LspClient>, last_used}>`, picks a slot by `language_id_for(uri)`, starts it on first use, and evicts idle slots on each call. `ToolRegistry` gets a pool instead of a single client. | FR-LSP-10. Replaces "first server that starts wins" (`tools/src/lib.rs`). |
| CE-DQ21 | **Symbol addressing resolution order** | (1) `CodeIndexPort::locate_symbol` (exact qualified name, then unqualified); (2) `workspace/symbol` on the pool for that language; (3) error with candidates. The resolved position is the *name identifier* of the definition, which is where LSP servers expect the cursor. | FR-LSP-05. |
| CE-DQ22 | **Compaction and the transcript's system message** | Compaction never touches `history[0]`. The repo map is *appended to the mode prompt* inside the same system message (`\n\n# Repository map\n…`), so the system message stays one block (BP2) and is stored in the session (`Session.repo_map`), so a resumed session rebuilds identical bytes. | FR-INDEX-08, FR-CACHE-02. Today a resume already rewrites `history[0]`, and it must do so *identically* (same mode, same map). |
| CE-DQ23 | **Evaluation harness** | New workspace member `evals/` (package `zcode-evals`, binary) that runs the built `zcode` binary headless for each corpus task, reads the report JSON, runs the task's grader command, and writes `evals/results/<label>.json`. Adds `infra-llm` record/replay (`ZCODE_LLM_RECORD=<dir>` / `ZCODE_LLM_REPLAY=<dir>`, keyed by a SHA-less FNV hash of the request body) for deterministic self-tests. Live runs are manual (`make eval-tokens LIVE=1`). | FR-BUDGET-07. Replay makes the harness itself testable without spending money. |
| CE-DQ23a | **Replay keying (amends CE-DQ23)** | Record/replay works on the engine-level `LlmEvent` stream, keyed by **call sequence** (`NNNN.jsonl`), not raw bodies keyed by a request hash. | A hash key goes stale whenever a tool description or budget changes, which is what the harness exists to measure. Wire-format correctness stays covered by the decoders' own `parse_*_events` tests. |

### 2.1 Answers to the PRD's open questions

| PRD Q | Answer |
|-------|--------|
| Q1 Index format | Custom versioned binary (CE-DQ17). |
| Q2 Repo map default | On by default at 1,024 tokens, **automatically off for repos with fewer than 200 indexed files**. Re-decided at the Phase 3 gate from evaluation data (task-33). |
| Q3 Provider-native context management | Not in this milestone (PRD §13). |
| Q4 tree-sitter versions and size | Measured (§3): `tree-sitter 0.26.13`, grammars `rust 0.24.2`, `go 0.25.0`, `typescript 0.23.2`, `python 0.25.0`. |
| Q5 Large-file read policy | First 400 lines plus outline (PRD default). The evaluation in task-26 reports adoption. |
| Q6 Gutter everywhere | Yes. Format `{n:>w}│` with `w` = digits of the last line shown, so the gutter is 2–5 tokens per line. |

---

## 3. Dependency Verification (measured 2026-09-28)

A scratch crate on **toolchain 1.85.0**, release profile identical to the
workspace's (`lto = "thin"`, `codegen-units = 1`, `panic = "abort"`,
`strip = "symbols"`), with `incompatible-rust-versions = "fallback"`:

| Crate | Resolved | Builds on 1.85? | Note |
|-------|----------|-----------------|------|
| `ignore` | 0.4.30 → **pin `=0.4.29`** | 0.4.30 ✗ (E0658 let-chains, `src/incremental.rs:274`); 0.4.29 ✓ | 0.4.31+ declare 1.88; 0.4.30 declares nothing but still fails |
| `globset` | 0.4.19 | ✓ | 0.4.20 declares 1.88 |
| `grep-searcher` / `grep-regex` / `grep-matcher` | 0.1.17 / 0.1.14 / 0.1.9 | ✓ | |
| `tree-sitter` | 0.26.13 | ✓ | 0.27 declares 1.90 |
| `tree-sitter-language` | 0.1.7 | ✓ | 0.1.8 declares 1.90 |
| `tree-sitter-{rust,go,typescript,python}` | 0.24.2 / 0.25.0 / 0.23.2 / 0.25.0 | ✓ | Needs a C compiler (`cc`); already needed on every supported target |
| `streaming-iterator` | 0.1.9 | ✓ | Required by the tree-sitter 0.26 `QueryCursor` API |

**Binary size (stripped release, isolated probe):** empty `main` 0.33 MB;
search stack 2.03 MB (+1.70 MB); search + tree-sitter + 5 grammars 6.87 MB
(+4.84 MB for tree-sitter). zcode already links `regex`, `memchr` and `aho-corasick`,
so the real delta is expected to be **≈ +5.5–6.0 MB on the 5.5 MB v0.6.0 binary**.
That is inside GR5 (≤ +6 MB), but tight. Each task that adds dependencies records
the measured `make size` delta in its PR, and task-32 records it per grammar.

**Walk + search smoke test:** 1,371 `fn \w+` matches across `crates/` in 0.77 s
wall on a cold filesystem cache (0.02 s user), single-threaded.

---

## 4. Crate Topology (after this milestone)

```
cli ──► app ──► domain                                   (domain stdlib-only; app: domain + thiserror)
cli ──► tools ──► infra/{filesystem, shell, config, mcp, lsp, search, index}
                                         infra-index ──► infra-search   (infra→infra allowed)
cli ──► infra/{llm, session, telemetry, config, search, index} ──► domain
evals (zcode-evals) ──► runs the `zcode` binary; depends on serde_json only
```

| Crate | New / changed | New third-party deps |
|-------|---------------|----------------------|
| `crates/infra/search` (`infra-search`) | **new** | `ignore =0.4.29`, `globset 0.4`, `grep-searcher 0.1`, `grep-regex 0.1`, `grep-matcher 0.1` |
| `crates/infra/index` (`infra-index`) | **new** | `tree-sitter 0.26`, `streaming-iterator 0.1`, grammar crates (feature-gated) |
| `evals` (`zcode-evals`) | **new** | none beyond the workspace (`serde`, `serde_json`, `toml`) |
| `domain`, `app` | changed | none (FR-DI-01/02) |
| all others | changed | none |

`docs/architecture/dependency-check.sh`: add `infra-search` and `infra-index` to
`INFRA_CRATES`, and update the header comment's topology. `evals` is excluded from
the CLI composition-root check.

---

## 5. Domain Changes (`crates/domain`, stdlib only)

### 5.1 `ports.rs`: usage and messages

```rust
pub struct LlmFinish {
    pub reason: LlmFinishReason,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,     // CE-DQ9, was: cache_tokens
    pub cache_write_tokens: u64,
    pub cost_usd: Option<f64>,
}
impl LlmFinish { pub fn cache_tokens(&self) -> u64 { self.cache_read_tokens + self.cache_write_tokens } }

pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    pub tool_calls: Box<[LlmToolCall]>,
    pub tool_result: Option<LlmToolResult>,
    pub meta: MessageMeta,          // CE-DQ3 — never serialised onto the wire
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessageMeta {
    pub step: u32,
    pub kind: MessageKind,          // Normal | Summary | Elided | RepoMap
    pub subject: Option<Subject>,
    pub tokens_est: u32,
    pub spill: Option<String>,      // relative path, if the full output was spilled
}

#[derive(Clone, Debug, PartialEq)]
pub enum Subject {
    FileRange { path: String, start: u32, end: u32, hash: u64 }, // read / view
    FileWrite { path: String },                                   // write / edit / patch
    Diagnostics { path: Option<String> },
    Listing { path: String },                                     // list_dir / glob
    Search { key: u64 },                                          // grep / symbols: hash of normalised args
}

pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub error: Option<String>,
    pub subject: Option<Subject>,   // set by the tool; the engine copies it into meta
}
```

Every constructor (`LlmMessage::system/user/…`, `ToolResult::ok`) gets
`..Default::default()` for the new fields, so existing call sites compile after a
mechanical update.

### 5.2 New ports

```rust
// search.rs
pub struct GrepQuery { pub pattern: String, pub literal: bool, pub root: PathBuf,
    pub globs: Box<[String]>, pub types: Box<[String]>, pub case: CaseMode,
    pub multiline: bool, pub context: u8, pub max_file_bytes: u64 }
pub struct GrepFileHit { pub path: String, pub count: u32, pub lines: Box<[GrepLine]> }
pub struct GrepLine { pub line: u32, pub text: String, pub is_context: bool, pub match_col: u32 }
pub struct GrepOutcome { pub files: Box<[GrepFileHit]>, pub skipped_large: u32, pub partial: bool }
pub struct GlobQuery { pub patterns: Box<[String]>, pub root: PathBuf, pub kind: EntryKind }
pub struct WalkEntry { pub path: String, pub is_dir: bool, pub modified_ns: u128, pub excluded: bool }

pub trait SearchPort: Send + Sync {
    fn grep(&self, q: &GrepQuery, cancel: &dyn Fn() -> bool) -> Result<GrepOutcome, BoxError>;
    fn glob(&self, q: &GlobQuery) -> Result<Box<[WalkEntry]>, BoxError>;
    fn list(&self, root: &Path, depth: u8) -> Result<Box<[WalkEntry]>, BoxError>;
    fn walk_files(&self, root: &Path) -> Result<Box<[WalkEntry]>, BoxError>; // index input
}

// code_index.rs
pub enum SymbolKind { Function, Method, Struct, Enum, Trait, Interface, Class, Type, Const, Module }
pub struct Span { pub start_line: u32, pub end_line: u32, pub start_byte: u32, pub end_byte: u32 }
pub struct SymbolDef { pub name: String, pub qualified: String, pub kind: SymbolKind,
    pub path: String, pub span: Span, pub name_line: u32, pub name_col: u32,
    pub signature: String, pub depth: u8 }
pub enum IndexState { Disabled, Building { done: u32, total: u32 }, Ready }

pub trait CodeIndexPort: Send + Sync {
    fn state(&self) -> IndexState;
    fn outline(&self, path: &str) -> Result<Option<Box<[SymbolDef]>>, BoxError>; // None = no grammar
    fn symbols(&self, query: &str, kind: Option<SymbolKind>, path_prefix: Option<&str>, limit: usize)
        -> Result<Box<[SymbolDef]>, BoxError>;
    fn related(&self, target: &str) -> Result<Related, BoxError>;
    fn locate(&self, path: Option<&str>, qualified: &str) -> Result<Box<[SymbolDef]>, BoxError>;
    /// Parse `text` as `path`'s language: symbol spans and syntax error count, bypassing the store.
    fn parse_text(&self, path: &str, text: &str) -> Result<Option<ParsedFile>, BoxError>;
    fn repo_map(&self, prompt: &str, budget_tokens: u32) -> Result<String, BoxError>;
    fn notify_changed(&self, path: &str);
}
pub struct ParsedFile { pub defs: Box<[SymbolDef]>, pub error_nodes: u32, pub first_error: Option<(u32, u32)> }

// ports.rs additions
pub trait SpillPort { fn spill(&mut self, session: &str, call_id: &str, content: &str) -> Result<String, BoxError>; }
pub trait SessionStorePort { /* existing … */
    fn archive(&mut self, id: &str, messages: &[LlmMessage]) -> Result<(), BoxError> { let _ = (id, messages); Ok(()) }
}
```

`Session` gets `pub compactions: Box<[CompactionRecord]>` and
`pub repo_map: Option<String>`.

**Defaulted trait additions.** Every one has a no-op default body, so the
existing fakes and adapters compile unchanged:

| Trait | Method | Default | Implemented in | Task |
|-------|--------|---------|----------------|------|
| `ToolRegistryPort` | `elide_args(&self, name, args_json) -> Option<String>` | `None` | `tools` (serde_json) | 28 |
| `ToolRegistryPort` | `classify_call(&self, name, args_json) -> Option<&'static str>` | `None` | `tools` | 31 |
| `LlmPort` | `set_session(&mut self, id: &str)` | no-op | `infra-llm` (`prompt_cache_key`) | 23 |
| `LlmPort` | `set_interactive(&mut self, on: bool)` | no-op | `infra-llm` (TTL policy) | 31 |
| `LspPort` | `diagnostics`, `workspace_symbols`, `readiness` | empty / `Ready` | `infra-lsp` | 30 |
| `SearchPort` | `walk_files_for_index(&self, root)` | `walk_files` | `infra-search` | 32 |
| `CodeIndexPort` | `notify_turn_start(&self)` | no-op | `infra-index` | 32 |
| `SessionStorePort` | `archive(&mut self, id, messages)` | `Ok(())` | `infra-session` | 28/29 |

`UiEvent` gets `Compacted { tier: u8, tokens_before: u64, tokens_after: u64 }`,
`CacheReset { reason: &'static str }` and `Context { live_tokens: u64, window: Option<u64> }`.
The last one is emitted after each `Usage` so the TUI can show `ctx 62%`.

### 5.3 `context.rs`: pure compaction policy (new)

```rust
pub struct Policy { pub keep_recent_steps: u32, pub elide_over_tokens: u32 }
pub struct Plan { pub supersede: Vec<usize>, pub elide: Vec<usize>, pub summarise: Option<Range<usize>> }

pub fn protected(history: &[LlmMessage], p: &Policy) -> Vec<bool>;       // FR-CTX-03
pub fn superseded(history: &[LlmMessage]) -> Vec<usize>;                 // FR-CTX-05 rules
pub fn elidable(history: &[LlmMessage], prot: &[bool], p: &Policy) -> Vec<usize>;
pub fn summarisable_span(history: &[LlmMessage], prot: &[bool]) -> Option<Range<usize>>; // step-aligned
pub fn stub_for(msg: &LlmMessage, call: Option<&LlmToolCall>) -> String;  // FR-CTX-06 text
pub fn validate(history: &[LlmMessage]) -> Result<(), String>;           // FR-CTX-04 invariants
```

Supersession rules (each has a unit test):

1. `FileRange{path, a..b}` at index *i* is superseded when a later message *j* has
   `FileRange{path, c..d}` with `c ≤ a && d ≥ b`, or `FileWrite{path}`.
2. `Diagnostics{p}` is superseded by a later `Diagnostics{p}`, or by `Diagnostics{None}` when `p` is `Some`.
3. `Search{key}` or `Listing{path}` is superseded by a later identical subject.

A superseded result's content becomes
`[superseded at step N: <reason>]`. The `tool_result` message stays (FR-CTX-04).

### 5.4 `tokens.rs`

`estimate_tokens` gets the new formula (CE-DQ11). It also gains `TokenCalibrator`
and `PromptSize::from_finish` (CE-DQ10). `estimate_tokens` keeps its signature,
so every existing caller improves with no change.

### 5.5 `modes.rs`

`write_tool_names()` adds `edit_symbol`. `lsp__rename_symbol` is already there. The
`apply: true` variant is gated by the same name. The new read-only tools (`grep`,
`glob`, `outline`, `symbols`, `related`, `lsp__diagnostics`) are **not** added
anywhere. Tests assert they are allowed in all three modes. The `planning`
prompt names the new read-only tools. All three prompts gain one shared,
fixed-text paragraph (the navigation protocol, PRD §4.1), with no volatile data.

---

## 6. App Changes (`crates/app`)

### 6.1 `AgentLoop::execute`: changed sections

| Where (v0.6.0) | Change |
|----------------|--------|
| Before the request is built (`estimated_prompt_tokens`) | Replace the words×4 sum with `self.ctx.live_tokens(&history)` (CE-DQ10). Call `self.ctx.maybe_compact(&mut history, &mut session, …)` **before** building `LlmRequest` (FR-CTX-01). |
| Usage accounting (`input_tokens += …`) | Accumulate read and write separately. Call `self.ctx.observe(&finish, &history)` to update `last_reported_prompt` and the calibrator. Emit `UiEvent::Context`. |
| Provider error path (`parse_window_from_error` already used) | On a context-length rejection: `learn`, `ctx.force_compact`, retry **once** (FR-CTX-10). A second rejection surfaces as today. |
| Tool dispatch (`truncate_tool_output`) | Replace with `shape_output(payload, budget_for(tool))`: head 40 % / tail 60 % (FR-READ-05). When cut and a `SpillPort` is set, spill the full text and put the path in the marker (FR-READ-07). Copy `ToolResult.subject` into `MessageMeta`, and set `meta.step`, `meta.tokens_est`, `meta.spill`. |
| Tool telemetry (`tool_result` event) | Add `tokens_est`, `chars`, `category`, `spilled` (FR-BUDGET-01). The category comes from `domain::tool_category(name)` (a new pure fn beside `canonical_tool_name`). |
| Session open | If `session.repo_map` is `None` and an index port is set, compute the map (FR-INDEX-08) and store it. Build the system message as `mode_prompt + "\n\n# Repository map\n" + map` (CE-DQ22). |
| Mode change on resume | If the stored `session.mode` differs from `req.mode`, emit `CacheReset{"mode"}` (FR-CACHE-08). |

`truncate_tool_output` stays as a public wrapper for back-compat tests. Its
semantics change to head+tail, and the existing tests are updated to assert the
tail survives.

### 6.2 `context.rs`: `ContextManager` (new)

```rust
pub struct ContextConfig { pub enabled: bool, pub compact_at: f64, pub compact_target: f64,
    pub compact_at_tokens: u64, pub keep_recent_steps: u32, pub summary_max_tokens: u32 }

pub struct ContextManager {
    cfg: ContextConfig,
    calibrator: TokenCalibrator,
    last_reported_prompt: Option<(u64 /*tokens*/, usize /*history len at that time*/)>,
    compactions_since_reset: u32,
}
impl ContextManager {
    pub fn live_tokens(&self, history: &[LlmMessage]) -> u64;
    pub fn observe(&mut self, finish: &LlmFinish, cache_within_input: bool, history: &[LlmMessage]);
    pub fn maybe_compact(&mut self, h: &mut Vec<LlmMessage>, s: &mut Session, window: Option<u64>,
        deps: &mut CompactDeps<'_>) -> Result<Option<CompactionRecord>, AppError>;
    pub fn force_compact(/* same */) -> …;
}
pub struct CompactDeps<'a> { pub llm: &'a mut (dyn LlmPort + Send), pub sessions: &'a mut (dyn SessionStorePort + Send),
    pub emitter: &'a mut (dyn Emitter + Send), pub model: &'a str, pub focus: Option<&'a str> }
```

Algorithm (`maybe_compact`):

```
live = live_tokens(h); limit = window.map(|w| w*compact_at).unwrap_or(compact_at_tokens)
if !enabled || live < limit { return None }
target = window.map(|w| w*compact_target).unwrap_or(compact_at_tokens*compact_target/compact_at)
prot   = domain::context::protected(h, policy)
archive_candidates = []
tier 1: for i in superseded(h): archive(h[i]); h[i].content = stub; meta.kind = Elided
        if live_tokens(h) <= target → done(tier=1)
tier 2: for i in elidable(h, prot) (oldest first): archive; replace; stop when <= target → done(tier=2)
tier 3: span = summarisable_span(h, prot)?; text = summarise(h[span], focus)   // CE-DQ5
        on Err → warn, fall through to emergency (FR-CTX-09)
        summary = LlmMessage::user(render_summary(text, files_touched_ledger(h[span])))  meta.kind=Summary
        archive(h[span]); h.splice(span, [summary]) → done(tier=3)
emergency: elide inside the protected window except the latest step; done(tier=2)
domain::context::validate(h)?   // debug_assert in release; test-enforced
sessions.archive(id, &archived)?; s.compactions.push(record); emit Compacted + CacheReset{"compaction"}
```

The summary goes in as a **user-role** message whose first line is
`[Session summary — steps 3–41 were compacted]`. That keeps role alternation valid
for Anthropic (a user message follows the first user message's step, and the next
item is an assistant message). A `validate` check with a fix-up merges it when two
user messages would become adjacent.

### 6.3 `App` wiring surface

New setters, all optional so every existing test harness still builds:
`set_spill(Box<dyn SpillPort + Send>)`, `set_code_index(Arc<dyn CodeIndexPort + Send + Sync>)`,
`set_compaction_llm(Box<dyn LlmPort + Send>)`, `set_context_config(ContextConfig)`,
`set_tool_budgets(ToolBudgets)`.
`ExecutionResult` replaces `cache_tokens` with `cache_read_tokens` and
`cache_write_tokens`, and adds `compactions: u32` and `peak_context_tokens: u64`.

---

## 7. Infra Changes

### 7.1 `infra-search` (new): `crates/infra/search/src/`

| File | Content |
|------|---------|
| `filter.rs` | `DiscoveryFilter::new(root, &FilterConfig)`: builds `ignore::WalkBuilder` settings (`hidden(false)`, `git_ignore(true)`, `git_global(true)`, `git_exclude(true)`, **`require_git(false)`**, `follow_links(false)`, `add_custom_ignore_filename(".zcodeignore")`) and an `ignore::overrides::Override` from `DEFAULT_EXCLUDES` (PRD Appendix C) + `context.exclude` as `!glob`, with `context.include` as whitelist globs. `fn explain(path) -> Option<Rule>` for `zcode ignore check`. `fn is_explicit_root_allowed` implements FR-FILTER-02: when the query root is itself excluded, the walker is built rooted *at* it with that override removed. |
| `defaults.rs` | `DEFAULT_EXCLUDES`, `SECRET_EXCLUDES`, `SECRET_ALLOW` (`.env.example`, …). |
| `grep.rs` | `grep()` uses `WalkBuilder::build_parallel()` with `threads = min(available_parallelism, 8)`. Each worker owns a `grep_searcher::Searcher` (binary detection `BinaryDetection::quit(b'\0')`, line numbers on, `multi_line` when asked) and a `RegexMatcher` built with `RegexMatcherBuilder` (`case_smart`, `size_limit(10 MB)`, `dfa_size_limit(10 MB)`, `fixed_strings` when `literal`). Hits are sent over `std::sync::mpsc` and collected into a `BTreeMap<path, GrepFileHit>` (deterministic, FR-SEARCH-05). Checks `cancel()` per file. Stops early once `max_collect = limit + offset + 1` files are gathered in `files` mode, but keeps *counting* totals up to a hard cap of 100 k matches (reported as "100000+"). |
| `glob.rs` | `glob()` uses one `globset::GlobSet` and the same walker. `list()` walks to a bounded depth and marks excluded directories with `excluded: true` (FR-FILTER-05) by testing the override on each directory the walker skips. |
| `heuristics.rs` | `looks_minified`, `looks_generated`, `is_lockfile` (FR-FILTER-06). They are used by `walk_files` for the index, and **not** by `grep`. |
| `lib.rs` | `pub struct RipgrepSearch { filter: DiscoveryFilter }` implementing `SearchPort`. Errors via `thiserror` → boxed at the boundary. |

### 7.2 `infra-index` (new): `crates/infra/index/src/`

| File | Content |
|------|---------|
| `lang.rs` | `Lang` enum, per-feature grammar table (`tree_sitter_rust::LANGUAGE.into()`, …), `for_path(ext)`. |
| `queries/*.scm` | Per-language `defs.scm` with captures `@def.function`, `@def.method`, `@def.class`, …, `@name`, `@doc` (attached comments), `@attr` (attributes and decorators), plus `imports.scm` and `idents.scm`. Embedded with `include_str!`. |
| `extract.rs` | `parse(lang, text, deadline) -> ParsedFile` uses `Parser::parse_with_options` with `ParseOptions::progress_callback`, whose closure returns `ControlFlow::Break(())` once 500 ms have passed (FR-INDEX-11; verified against the tree-sitter 0.26.13 source). Query iteration uses `QueryCursor::matches` + `streaming_iterator::StreamingIterator`. It builds `SymbolDef`s: span = node ∪ contiguous leading `@doc`/`@attr` siblings; `qualified` from the ancestor walk (CE-DQ15); signature = first line of the node, trimmed, clipped to 200 chars. It counts `ERROR` and `MISSING` nodes. |
| `graph.rs` | Occurrence maps, edges, PageRank, repo-map render (CE-DQ18). |
| `store.rs` | Format and atomic persistence (CE-DQ17). `Snapshot { files: Vec<FileRec>, by_name: HashMap<Box<str>, Vec<(u32,u32)>> }`. Strings are interned `Box<str>`. |
| `builder.rs` | `spawn_build(root, search: Arc<dyn SearchPort>, store_path) -> Arc<CodeIndex>`: loads the store, stats files, re-parses changed ones (mtime/size, then FNV hash), publishes batches of 200 files into `RwLock<Snapshot>` so early queries see partial data, then persists. It runs on a `std::thread` named `zcode-index` (FR-INDEX-01). |
| `lib.rs` | `CodeIndex` implements `CodeIndexPort`. Each query `stat`s the files it touches and re-parses stale ones inline (FR-INDEX-04). `notify_changed` re-parses synchronously. `outline(path)` for a directory returns the top-level defs of each file, one level deep. Fallback outline (regex, labelled approximate) lives in `tools`, not here: this crate returns `None` for unsupported languages. |

### 7.3 `infra-lsp`

- **CE-DQ19** notification routing in `send_request`; new
  `pub fn diagnostics(&mut self, uri: Option<&str>, settle: Duration, max: Duration) -> Vec<(String, Diag)>`;
  pull diagnostics when `capabilities.diagnosticProvider` is present (the
  initialise result is kept in `self.capabilities: Value`).
- `workspace_symbol(query) -> Vec<LspSymbol>`; `progress_percent() -> Option<u8>`.
- **CE-DQ20** `LspPool` in `pool.rs`, implementing `LspPort`, plus the extra
  inherent methods above, forwarded to the right slot.
- `domain::LspPort` gets default-implemented additions: `diagnostics`,
  `workspace_symbols`, `readiness`. The defaults return empty or `Ready`, so fakes
  in tests do not change.

### 7.4 `infra-llm`

- `build_anthropic_request(req, model, CacheLayout)`, where
  `CacheLayout { ttl_head: Ttl, ttl_tail: Ttl }`: BP1 to BP4 per CE-DQ6/DQ7.
  Blocks that can carry markers: `text`, `tool_use`, `tool_result`, `image`. If
  the target message's last block is not markable, walk back within that message.
- `build_openai_request(req, model, markers: CacheMarkers, cache_key: Option<&str>)`
  per CE-DQ8. System-message marker: convert `content` to
  `[{type:"text", text, cache_control}]` only when markers are on.
- Decoders: `OpenAiDecoder` reads `prompt_tokens_details.cached_tokens` → read,
  `prompt_cache_hit_tokens` (DeepSeek) → read. `AnthropicDecoder` splits
  `cache_creation_input_tokens` → write and `cache_read_input_tokens` → read, and
  stops reading `cache_creation_output_tokens`, which is not in Anthropic's
  documented usage object. It is summed today only if present, and the canned
  test fixture that includes it is corrected. The optional
  `usage.cache_creation.{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}`
  breakdown is read into telemetry (not into `LlmFinish`) for the TTL policy in
  task-31.
- `Ollama`: `keep_alive` from config (FR-CACHE-10).
- The `LlmPort` constructors gain `with_session_id(&str)` (for `prompt_cache_key`),
  called by `App` before each run through a new defaulted
  `LlmPort::set_session(&mut self, id: &str) {}`.
- Record/replay (CE-DQ23): `RecordingTransport` wraps `send_with_retry` and stores
  the response bodies. `ReplayLlm` serves them. Both are selected in `cli::wire`
  from the env vars.

### 7.5 `infra-session`

`SCHEMA_VERSION = 2`. `SessionFile` gains `compactions: Vec<CompactionRecordFile>`
(`#[serde(default)]`), `repo_map: Option<String>`, and per-message optional
`meta` (`#[serde(default, skip_serializing_if = "MetaFile::is_default")]`).
`version: 1` files load (every new field defaults). `archive()` appends one JSON
line per message to `<id>.archive.jsonl`. `export_to` gains `full: bool`, which
inlines the archive as `archived_messages`. `import_from` accepts both.

### 7.6 `infra-telemetry`

- Events and totals: `cache_read_tokens`, `cache_write_tokens` (the JSONL keeps a
  derived `cache_tokens` for one release, marked deprecated in CHANGELOG).
- Aggregation (FR-BUDGET-04): `ToolLedger { by_tool: BTreeMap<String, Agg>, by_category: BTreeMap<String, Agg> }`,
  fed from `tool_result` extras. The report gets `context: { peak_tokens, peak_pct, compactions, reclaimed_tokens }`
  and `cache: { read, write, hit_ratio }`.
- `opencode.rs`: `tokens.cache.read/write` get the real split (it currently hardcodes `write: 0`).
  `context_compacted` is not translated, because opencode defines no equivalent
  event (translation, not emulation).

### 7.7 `infra-filesystem`

`SpillStore::new(working_dir, ttl_days)` → `.zcode/spill/`. It prunes at
construction and writes atomically with the existing `write_atomic`. It
implements `domain::SpillPort`.

### 7.8 `infra-config`

New sections exactly as PRD §7. The `FileConfig` mirrors are all
`Option`/`#[serde(default)]`, merged per field like the others. Env vars:
`ZCODE_CONTEXT_COMPACTION`, `ZCODE_CONTEXT_COMPACT_AT`, `ZCODE_INDEX_ENABLED`,
`ZCODE_CACHE_TTL`, `ZCODE_READ_DEFAULT_LIMIT`. Validation: `0 < compact_target < compact_at < 1`,
TTL ∈ {5m, 1h, auto}, `syntax_check` ∈ {reject, warn, off}; an invalid value is a
`ConfigError` that names the key. `zcode config` prints every new effective value.

---

## 8. Tools Changes (`crates/tools`)

| File | Change |
|------|--------|
| `render.rs` (**new**) | CE-DQ13 helpers. |
| `native.rs` | `ReadTool` gets ranges, gutter, guard (asks the index for the outline), long-line clip, binary refusal, `subject`. `ListDirTool` delegates to `SearchPort::list`. `StrReplaceTool`: `view_range`; FR-EDIT-06/07 (ambiguity error with line numbers, normalised retry, closest region by line-level similarity); `subject = FileWrite`; no-echo results. `WriteTool`: diffstat plus the >200-line hint. |
| `search_tools.rs` (**new**) | `GrepTool`, `GlobTool` (hold `Arc<dyn SearchPort>` plus a `CancelFlag` clone). |
| `index_tools.rs` (**new**) | `OutlineTool`, `SymbolsTool`, `RelatedTool` with the regex fallback (FR-INDEX-09). |
| `edit_symbol.rs` (**new**) | `EditSymbolTool` (FR-EDIT-01..05): read the file → `parse_text` (fresh, never stored spans; PRD R6) → locate → splice → re-indent → `parse_text` of the result → syntax gate → `write_atomic` → `notify_changed` → LSP sync → optional diagnostics delta. |
| `lib.rs` | `ToolRegistry` gets `with_search`, `with_code_index`, and `with_lsp_pool`. LSP position tools accept `symbol` (CE-DQ21), compact rendering, `lsp__diagnostics`, rename `apply`. **The post-edit hook** (`after_write(path)`) runs for every native write tool: index `notify_changed` + LSP `open_document` with the new text (FR-LSP-09, FR-INDEX-04). MCP tool specs are sorted by (server, tool) (FR-CACHE-02). `from_config` builds and shares the search and index services. |
| `patch.rs` | `apply_patch` returns a per-file diffstat. It calls `after_write` for each touched file. |

**Tool registration order** (and therefore wire order, which must be fixed):
`read, list_dir, glob, grep, outline, symbols, related, write, str_replace_editor,
edit_symbol, apply_patch, shell, zcode_skill, lsp__*, mcp__*`.
A snapshot test pins the order.

---

## 9. CLI Changes (`crates/cli`)

- `wire_with_format`: builds `RipgrepSearch`, spawns `CodeIndex` (unless disabled),
  `SpillStore`, and the optional compaction client (resolved like `--model`); sets
  the context config and tool budgets. Record/replay env vars (CE-DQ23).
- Flags: `--no-index`, `--no-compact`. Subcommands: `zcode index {status,rebuild,clear}`,
  `zcode session compact <id> [--focus <text>]`, `zcode session export --full`,
  `zcode ignore check <path>`.
- TUI: `/context` (FR-BUDGET-05), `/compact [focus]` (FR-CTX-12), a status-bar
  `ctx NN%` item fed by `UiEvent::Context`, notes for `Compacted` and `CacheReset`,
  and `/cost` extended with read, write, hit ratio and saving (FR-BUDGET-06).
  `/context` needs a snapshot of the live history: `EngineMsg` gets a
  `ContextBreakdown` variant that the engine thread sends in reply to a request
  message on the existing channel pattern.
- `emit.rs`: prints compaction and cache-reset lines in pretty mode. JSONL passes
  them through.

---

## 10. Data and Schema Migrations

| Artifact | v0.6 | After | Compatibility |
|----------|------|-------|---------------|
| Session file | `version: 1` | `version: 2` (+`compactions`, `repo_map`, message `meta`) | v1 loads; v2 is not readable by v0.6 (documented) |
| Session archive | — | `sessions/<id>.archive.jsonl` | new file |
| Spill | — | `.zcode/spill/<sid>/<call>.txt` | new dir, pruned |
| Index | — | `.zcode/index/v1/index.zcix` | discarded and rebuilt on version mismatch |
| JSONL `zcode` | `cache_tokens` | `cache_read_tokens`, `cache_write_tokens`, `cache_tokens` (deprecated, = sum) | additive |
| Report JSON | totals | + `by_tool`, `by_category`, `context`, `cache` | additive |
| `opencode` JSONL | `cache.write: 0` | real split | field-compatible |

---

## 11. Cross-Cutting Rules

1. **Determinism:** no `HashMap` iteration reaches output. Use `BTreeMap` or
   sort first. A `determinism` test module in each new crate runs the operation 20×
   and compares bytes.
2. **Errors:** model-fixable problems → `Ok(ToolResult{error: Some})`;
   infrastructure → `Err` (CLAUDE.md tool convention). The index and search services
   never return `Err` for "no results".
3. **No panics on input** (`panic = "abort"`): no `unwrap`/`expect` on data from
   files, the model, or servers. Clippy lint `clippy::unwrap_used` is enabled
   (`#![deny]`) in `infra-search`, `infra-index`, `tools::edit_symbol`.
4. **Memory:** strings in long-lived structures are `Box<str>`, line numbers `u32`.
   `CodeIndex::heap_bytes()` exists so a test can hold the index to NFR-CTX-MEM-02,
   following the `Timeline::heap_bytes` precedent.
5. **Citations:** code cites `FR-*` from PRD-CTX-EFF-003 and `CE-DQn` from this plan.
   The existing `FR-COST-01` comments in `infra-llm` are re-pointed to `FR-CACHE-01/03`.

---

## 12. Testing Strategy

| Level | What | Where |
|-------|------|-------|
| Pure unit | Token estimator and calibrator, supersession / elision / protected set, stub text, `validate`, pricing, prompt size | `domain` |
| Property | Random transcripts (seeded stdlib xorshift, no `proptest` dependency): compaction keeps protected bytes, never orphans tool calls, and always serialises through the 4 builders | `app` (builders via golden JSON fixtures produced by `infra-llm` tests and checked in under `crates/app/tests-data/`), `infra-llm` |
| Golden payload | Anthropic BP1–BP4 placement over a 5-step session; prefix-byte-stability across steps; OpenAI markers and `prompt_cache_key`; TTL variants | `infra-llm` |
| Decoder | Canned SSE per provider with cache usage | `infra-llm` |
| Fixture repos | `crates/infra/search/testdata/repo/` (gitignore layers, node_modules, .env, binary, huge file, symlink loop); `crates/infra/index/testdata/{rust,go,ts,py}/` | new crates |
| Tool snapshots | Output text of every new or changed tool, including footers | `tools` |
| Integration (`#[ignore]`) | rust-analyzer diagnostics, rename apply, workspace symbols; Anthropic / OpenAI cache hit on the second request | `infra-lsp`, `infra-llm` |
| Bench | `grep` vs `rg` on a generated 100 k-file tree; index cold / warm build on 10 k files; tiers 1+2 on a 150 k-token transcript | `benches` (criterion, `zcode-benches`) |
| Eval | Corpus runs, baseline diff | `evals` (`make eval-tokens`) |

`make ci` stays hermetic and adds nothing that needs the network or a language
server.

---

## 13. Observability, Performance, Security

- New telemetry kinds: `context_compacted`, `cache_reset`, `index_ready {files, symbols, ms}`,
  `lsp_server_started {language}`. Existing kinds gain fields (§10).
- Performance budgets are enforced by criterion benches (NFR-CTX-PERF-*). Each is
  run in its task, and its result recorded in the PR description.
- Security: no new process spawning except configured LSP servers; secret
  excludes (FR-FILTER-03); `.zcode/spill` and archives stay under the
  already-gitignored `.zcode/`; the summariser's system prompt states that the
  transcript is untrusted data.

---

## 14. Implementation Roadmap

```
Phase 0 (v0.6.x)   task-21 ─┬─► task-22
                            │
Phase 1 (v0.7.0)            ├─► task-23 (cache)
                            ├─► task-24 (infra-search) ─► task-25 (grep/glob/list)
                            ├─► task-26 (read shaping) ◄── uses task-24 for list_dir only
                            └─► task-27 (edit safety)
Phase 2 (v0.8.0)   task-21 + task-26 ─► task-28 (ctx tiers 1-2) ─► task-29 (tier 3 + persistence)
                   task-30 (LSP addressing/diagnostics)   task-31 (cache polish + /context /cost) ◄── task-23
Phase 3 (v0.9.0)   task-24 ─► task-32 (infra-index) ─┬─► task-33 (index tools + repo map)
                                                     ├─► task-34 (edit_symbol) ◄── task-27
                                                     └─► task-35 (LSP pool/readiness/rename) ◄── task-30
```

Branches: `develop-release-context-efficiency-p0` … `-p3`. Each phase closes with
an evaluation run (task-22 harness) against the PRD §11 exit gate, recorded in
`docs/prd/context-efficiency/code-review.md` together with the review.

---

## 15. Traceability (FR → task → primary files)

| FR group | Task | Primary files |
|----------|------|---------------|
| FR-BUDGET-01..04 | 21 | `domain/src/tokens.rs`, `domain/src/naming.rs` (`tool_category`), `app/src/lib.rs`, `infra/telemetry/src/lib.rs` |
| FR-BUDGET-07, §10 | 22 | `evals/`, `infra/llm/src/record.rs`, `Makefile` |
| FR-CACHE-01..06 | 23 | `infra/llm/src/lib.rs`, `domain/src/ports.rs`, `domain/src/pricing.rs`, `infra/telemetry/src/{lib,opencode}.rs`, `infra/session/src/lib.rs` |
| FR-FILTER-01..05 | 24 | `infra/search/src/{filter,defaults}.rs` |
| FR-SEARCH-01..07, FR-GLOB-01..02 | 25 | `infra/search/src/{grep,glob}.rs`, `tools/src/{search_tools,render}.rs` |
| FR-READ-01..05, 07 | 26 | `tools/src/native.rs`, `app/src/lib.rs`, `infra/filesystem/src/spill.rs` |
| FR-EDIT-06..08 | 27 | `tools/src/{native,patch}.rs` |
| FR-CTX-01..06, 09, 10 | 28 | `domain/src/context.rs`, `app/src/context.rs`, `app/src/lib.rs` |
| FR-CTX-07, 08, 11, 13 | 29 | `app/src/context.rs`, `infra/session/src/lib.rs`, `cli/src/cli/{mod,emit}.rs`, `cli/src/cli/tui/*` |
| FR-LSP-05..07, 09 | 30 | `infra/lsp/src/lib.rs`, `tools/src/lib.rs` |
| FR-CACHE-07..09, FR-BUDGET-05..06, FR-READ-06, FR-SEARCH-08..09 | 31 | `infra/llm`, `cli/src/cli/tui/{mod,command}.rs`, `infra/config` |
| FR-INDEX-01..04, 09..11, FR-FILTER-06 | 32 | `infra/index/src/*`, `infra/search/src/heuristics.rs` |
| FR-INDEX-05..08 | 33 | `tools/src/index_tools.rs`, `app/src/lib.rs`, `infra/index/src/graph.rs` |
| FR-EDIT-01..05, 09 | 34 | `tools/src/edit_symbol.rs` |
| FR-LSP-08, 10..12, FR-CTX-12, FR-FILTER-07 | 35 | `infra/lsp/src/pool.rs`, `tools/src/lib.rs`, `cli` |

---

## 16. Implementation Risks (beyond PRD §12)

| Risk | Mitigation |
|------|------------|
| `LlmMessage` gaining a field touches every constructor and pattern across the workspace | Land it first in task-21 as a mechanical change: `meta` defaulted, no behaviour change, one PR. |
| Anthropic rejects `cache_control` on some block type we mark | The golden tests cover every block type. The builder walks back to the nearest markable block. The integration test runs a real 5-step tool session. |
| The `ignore` pin blocks a future security fix | Pin is `=0.4.29` with a `# CE-DQ1` comment. Revisit when the toolchain pin moves to ≥ 1.88 (tracked in CHANGELOG *Known limitations*). |
| tree-sitter parse of a huge generated file stalls a turn | 1 MB cap, heuristics, 500 ms cancellation. `notify_changed` on such a file becomes a no-op. |
| TUI `/context` needs history from the engine thread | Request/response over the existing `EngineMsg` channel. No shared mutable history (keeps the "one channel" rule). |

---

*End of plan.*
