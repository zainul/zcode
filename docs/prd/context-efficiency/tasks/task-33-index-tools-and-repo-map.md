# Task 33 — Index Tools (`outline`, `symbols`, `related`), Regex Fallback, Repo Map

**Related PRD sections:** §5.4 FR-INDEX-05..09; §5.8 FR-READ-02 (outline in the large-file guard); §5.2 FR-LSP-05 (index-backed addressing); §14.2 Q2
**Technical plan:** CE-DQ13, CE-DQ18, CE-DQ21, CE-DQ22, §2.1 (Q2 answer), §6.1, §8
**Depends on:** task-32 (CodeIndex), task-26 (read guard hook), task-30 (symbol resolution seam)
**Phase:** 3 (v0.9.0)
**Status:** Done (v0.7.0)
**Priority:** High. This is where the index turns into fewer tokens.

## Objective

1. Three read-only tools on the index: `outline` (signatures and spans, no bodies), `symbols` (find definitions by name), and `related` (the file/symbol neighbourhood).
2. A regex fallback, so these tools answer (labelled approximate) when the index is not ready or has no grammar (FR-INDEX-09).
3. The ranked, token-budgeted **repo map**, frozen per session in the system message (CE-DQ18, CE-DQ22).
4. Plug the index into the `read` large-file guard and into LSP symbol resolution.

## Step-by-step

### 1. `tools/src/index_tools.rs`

**`outline`**. Params: `path`*. Output:

```
crates/app/src/lib.rs  (1,548 lines)
  struct ExecutionRequest                                   L40-70
  impl ExecutionRequest
    fn new(prompt: impl Into<String>) -> Self               L56-69
  trait AgentLoop                                           L90-98
  impl AgentLoop for App
    fn execute(&mut self, ctx: &AgentContext, req: …)       L386-819
  fn truncate_tool_output(content: String, max_chars: usize) L831-840
[12 definitions]
```

Indentation by `depth`. Signatures are clipped so the span column aligns at 60 (the right-aligned `Lstart-end`). A directory path lists each file with its top-level defs (at most 15 per file, `(+N more)`). Cap 300 lines + footer. `subject: Listing{path}`.

**`symbols`**. Params: `query`*, `kind` (enum), `path` (prefix), `limit` (20). Output `kind  qualified — path:start-end  signature`, one per line, then the footer.

**`related`**. Params: `path` or `symbol` (one required). Output sections `imports`, `imported by`, `defined in` (for a symbol), `referenced in ≈` (name-based, each `path:line`), each capped at 20. The last line: `for exact references use lsp__find_references symbol="…"`.

All three: when `state()` is `Building{done, total}`, append `(index building: done/total files — results may be incomplete)`.

### 2. Regex fallback (FR-INDEX-09)

`tools/src/index_fallback.rs`: per-extension definition regexes (Rust `^\s*(pub(\(.*?\))?\s+)?(async\s+)?(fn|struct|enum|trait|type|mod|const|impl)\b`, Go `^func |^type `, TS/JS `^(export\s+)?(default\s+)?(async\s+)?(function|class|interface|type|enum|const)\b`, Python `^\s*(async\s+)?(def|class)\b`, and a generic set for others).

- `outline` fallback: scan the file, and emit matching lines with their line numbers (no end line, shown as `L120`), labelled `(approximate — no parser for this language)` or `(approximate — index not ready)`.
- `symbols` fallback: `SearchPort::grep` with a combined definition regex anchored on the query (`\b(fn|func|def|class|struct|interface|type|trait|enum)\s+<query>\b`, `output=content`), rendered in the same line format, labelled approximate.

### 3. Repo map (FR-INDEX-08; `infra-index/src/graph.rs`)

```rust
pub fn rank(snapshot: &Snapshot, prompt: &str) -> Vec<(u32 /*file*/, f64)>;
pub fn render(snapshot: &Snapshot, ranked: &[(u32, f64)], budget_tokens: u32) -> String;
```

- **Edges:** for each identifier occurrence `n` in file A where B defines `n` (B ≠ A): weight += `1 / ln(2 + defs_named(n))`. Identifiers shorter than 3 chars and a small stoplist (`new`, `get`, `set`, `len`, `err`, `ctx`, `self`, `this`, `init`, `main`) are skipped. Aggregate into a sparse `Vec<Vec<(u32, f32)>>`.
- **Personalisation:** tokenise the prompt into words ≥ 3 chars. A file gets teleport weight 10 if its path contains a word, and 5 for each definition name that equals a word. Everything else gets 1. Normalise.
- **PageRank:** damping 0.85, 20 iterations, dangling mass redistributed by the teleport vector. `f64`, with sorting by (rank desc, path asc) for determinism.
- **Render:** a header `Files ranked by relevance; signatures only. Use outline/read for detail.`, then for each file in rank order: `path` and its `depth == 0` definitions' signatures (at most 8 per file). Stop before the running `estimate_tokens` exceeds the budget.
- `CodeIndex::repo_map(prompt, budget)` returns `""` when the index has fewer than **200** indexed files (the Q2 answer) or `budget == 0`.
- If the index is still `Building` when the session starts: wait up to **1.5 s** for `Ready` (bounded, so time-to-first-request grows by at most 1.5 s, and only on a cold first run). Otherwise use the partial snapshot, and note `(partial)` in the map header. It is frozen either way (CE-DQ22).

### 4. Engine integration (`app`)

In `execute`, before the system message is built:

```rust
if session.repo_map.is_none() {
    if let Some(ix) = &self.code_index {
        let map = ix.repo_map(&req.prompt, self.ctx_cfg.repo_map_tokens)?;   // errors → log, None
        session.repo_map = Some(map);                                       // "" means "decided: none"
    }
}
let system_text = match session.repo_map.as_deref() {
    Some(m) if !m.is_empty() => format!("{}\n\n# Repository map\n{}", modes::system_prompt(req.mode), m),
    _ => modes::system_prompt(req.mode).to_string(),
};
```

- Storing `Some("")` records "decided: no map", so a resume never computes one later and changes the prefix.
- `meta.kind = RepoMap` is **not** used for the system message (it stays `Normal`). The map is part of `history[0]`, which compaction never touches.
- `turn start`: `ix.notify_turn_start()` (task-32 rescan).

### 5. `read` large-file guard (FR-READ-02 completion)

`ReadTool` gets an `Option<Arc<dyn CodeIndexPort + …>>`. When the guard triggers, it appends up to 40 lines of `outline(path)` under `outline of the rest:` (only definitions starting after the last shown line).

### 6. LSP symbol resolution (task-30 seam)

In `resolve(Target::Symbol)`: try `index.locate(path, symbol)` first. For exactly one match, use `(name_line, name_col)` converted to wire. For several, return candidates. For none, fall back to `workspace/symbol`.

### 7. Config

`[index] enabled`, `repo_map_tokens = 1024`, `max_file_bytes`. `--no-index` disables both the tools (they fall back) and the map.

## Tests

- Snapshots: `outline` per fixture language and for a directory; `symbols` ranking (exact > prefix > fuzzy); `related` sections.
- `outline_is_under_15pct_of_full_read_tokens` over the fixture corpus (FR-INDEX-05 acceptance).
- `tools_label_building_state`.
- `fallback_outline_and_symbols_when_index_disabled` (FR-INDEX-09).
- Graph: `pagerank_is_deterministic`; `prompt_mentions_boost_files`; `common_names_are_downweighted`; `render_respects_budget` (estimated tokens ≤ budget).
- `repo_map_empty_below_200_files`.
- `repo_map_frozen_and_reused_on_resume` (a resume with a different prompt → the same `history[0]` bytes).
- `system_message_is_byte_identical_across_steps_with_map` (extends the task-23 prefix test).
- `read_guard_appends_outline_of_the_rest`.
- `lsp_symbol_resolution_prefers_index`.

## Test-case scenario

Next.js fixture, prompt "add a loading state to the checkout page". The repo map (1 k tokens) ranks `app/checkout/page.tsx` and `components/CheckoutForm.tsx` near the top, because the prompt mentions "checkout". The model calls `outline app/checkout/page.tsx` (≈ 250 tokens) and `read offset=40 limit=60`, and edits. It never lists `node_modules` or reads an unrelated file.

## How to verify

```sh
cargo test -p tools outline symbols related index_fallback read_guard
cargo test -p infra-index graph
cargo test -p app repo_map
make ci
make eval-tokens LIVE=1 LABEL=p3-index
```

**Pass criteria:** tests green; the evaluation shows the `inspect`-category tokens falling against Phase 2; the Q2 decision (repo map default) is confirmed or revised from the with/without comparison (run once with `repo_map_tokens=0`), and recorded in `code-review.md` and PRD §14.2.

## Success metric mapping

M2, M3, GR1 (locate tasks). FR-INDEX-05..09, FR-READ-02, FR-LSP-05.
