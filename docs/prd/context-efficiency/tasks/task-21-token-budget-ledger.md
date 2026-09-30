# Task 21 — Token Budget Ledger, Calibrated Estimator, Message Metadata

**Related PRD sections:** §5.0 FR-BUDGET-01..04; §1.1 B11; §2.3 (metric definitions)
**Technical plan:** CE-DQ3, CE-DQ10, CE-DQ11, §5.1, §5.4, §6.1, §7.6
**Depends on:** — (first task of the milestone)
**Phase:** 0 (v0.6.x)
**Status:** Done (v0.7.0)
**Priority:** Critical. Every later task's acceptance is measured with this. It also lands the `LlmMessage.meta` field that tasks 26, 28, 29 and 33 build on.

## Objective

Make every token attributable, **without changing what the model sees**:

1. Add `MessageMeta` to `LlmMessage` and `subject` to `ToolResult` (CE-DQ3), as a mechanical, behaviour-neutral change.
2. Replace the `words × 4` estimator with the calibrated estimator (CE-DQ11), and anchor the live-context figure on provider-reported usage (CE-DQ10).
3. Attach `tokens_est`, `chars`, `category`, `spilled` to every `tool_result` event, and aggregate them in the report file.

The v0.6.0 baseline (task-22) is captured **with this task applied and nothing else**, so its only effect must be measurement.

## Step-by-step

### 1. `crates/domain/src/ports.rs`: metadata types

Add `MessageMeta`, `MessageKind`, `Subject` exactly as in technical plan §5.1. Derive `Clone, Debug, Default, PartialEq` (`MessageKind::Normal` is the default).

```rust
pub struct LlmMessage { /* existing fields */ pub meta: MessageMeta }
pub struct ToolResult { /* existing fields */ pub subject: Option<Subject> }
```

Update every constructor in `ports.rs` (`LlmMessage::system/user/assistant/tool_result_message`, `ToolResult::ok/err`) to fill the defaults. Then run `cargo build --workspace` and fix each literal struct construction the compiler reports (fakes in `app` tests, `tools`, `infra-session`, TUI tests). This is a mechanical change: add `meta: Default::default()` / `subject: None`.

### 2. `crates/domain/src/tokens.rs`: estimator and calibrator

```rust
const CODE_DIVISOR: f64 = 3.6;
const PROSE_DIVISOR: f64 = 4.2;
const CODE_SYMBOL_SHARE: f64 = 0.12;

pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() { return 0; }
    let (mut chars, mut symbols) = (0u64, 0u64);
    for c in text.chars() { chars += 1; if c.is_ascii_punctuation() { symbols += 1; } }
    let d = if symbols as f64 / chars as f64 > CODE_SYMBOL_SHARE { CODE_DIVISOR } else { PROSE_DIVISOR };
    (chars as f64 / d).ceil() as u64
}

pub struct TokenCalibrator { ratio: f64 }
impl TokenCalibrator {
    pub fn new() -> Self { Self { ratio: 1.0 } }
    pub fn observe(&mut self, reported: u64, estimated: u64) { /* EMA α=0.3, clamp [0.5,2.0], ignore estimated==0 */ }
    pub fn apply(&self, estimated: u64) -> u64 { (estimated as f64 * self.ratio).round() as u64 }
}

pub struct PromptSize;
impl PromptSize {
    /// CE-DQ10: true prompt size of a finished call.
    pub fn from_finish(f: &LlmFinish, cache_within_input: bool) -> u64 {
        if cache_within_input { f.input_tokens } else { f.input_tokens + f.cache_tokens() }
    }
}
```

Until task-23 splits the cache fields, `f.cache_tokens()` is a one-line helper that returns the existing `cache_tokens` field. Add it now so task-23 only changes its body.

Expose `PriceTable::cache_within_input(model) -> bool` (default `false` for unknown models, which is Anthropic-like and never under-counts) in `domain/src/pricing.rs`, so the engine can call `from_finish` correctly.

### 3. `crates/domain/src/naming.rs`: `tool_category`

```rust
pub fn tool_category(name: &str) -> &'static str {
    match canonical_tool_name(name).as_str() {
        "list_dir" | "glob" => "discover",
        "grep" | "symbols" | "related" => "locate",
        "read" | "outline" | "lsp__hover" | "lsp__goto_definition" | "lsp__find_references" => "inspect",
        "write" | "str_replace_editor" | "apply_patch" | "edit_symbol" | "lsp__rename_symbol" => "change",
        "lsp__diagnostics" => "verify",
        "shell" => "shell",
        n if n.starts_with("mcp__") => "mcp",
        _ => "other",
    }
}
```

Tools that do not exist yet are listed now so later tasks need no edit here. `str_replace_editor view` is classified `change`. That small mis-attribution is documented in the test, and task-26 refines it by argument once `view` gets ranges.

### 4. `crates/app/src/lib.rs`: live context and attribution

- Add `calibrator: TokenCalibrator` and `last_prompt: Option<(u64, usize)>` (reported prompt tokens, `history.len()` at that time) to the per-run state.
- New private fn `live_tokens(&history)`: when `last_prompt = Some((t, n))`, return `t + calibrator.apply(Σ estimate(history[n..]))`. Otherwise return `calibrator.apply(Σ estimate(all))`. Replace **both** `estimated_prompt_tokens` sums (request clamping, and the input-token fallback) with it.
- After each `Finish`: `reported = PromptSize::from_finish(&finish, self.pricing.cache_within_input(&session.model))`. When `reported > 0`, call `calibrator.observe(reported, Σ estimate(request messages))` and set `last_prompt = Some((reported, history_len_sent))`.
- Tool dispatch: set `meta.step`, `meta.tokens_est` on the pushed tool-result message, and copy `ToolResult.subject` into `meta.subject`. Add these to the `tool_result` telemetry extras: `tokens_est` (Number), `chars` (Number), `category` (Text, via `tool_category`), `spilled` (Bool, always `false` until task-26).
- `ExecutionResult`: add `peak_context_tokens: u64`, the maximum of `live_tokens` seen before each request.

### 5. `crates/infra/telemetry/src/lib.rs`: aggregation

- `ToolLedger { by_tool: BTreeMap<String, Agg>, by_category: BTreeMap<String, Agg> }` where `Agg { calls: u64, tokens_est: u64, chars: u64, truncated: u64, errors: u64 }`. Fed from `tool_result` events' extras in `emit`.
- The report JSON gains `"ledger": { "by_tool": {...}, "by_category": {...} }` and `"context": { "peak_tokens": N, "peak_pct": F|null }`. `peak_pct` is present only when the window is known, and is passed in through a new `flush_report` extra. The existing fields are unchanged.

### 6. `crates/infra/session/src/lib.rs`: persist `meta` (optional)

Add `#[serde(default, skip_serializing_if = "MetaFile::is_default")] meta: MetaFile` to `SerializableMessage`, with a round-trip conversion. **Do not** bump `SCHEMA_VERSION` here: the field is optional, so v1 readers ignore it and v1 files load. Task-29 bumps the version when compaction records are added.

## Tests

- `domain::tokens`: `estimator_is_within_25pct_on_fixture_set`. A table of 20 `(text, known_tokens)` pairs (Rust, TS, Python, JSON, English prose) with counts taken once from the provider token-counting endpoint and committed as constants. Assert mean absolute percentage error ≤ 25 %.
- `calibrator_converges_to_10pct`: feed `(reported = 1.3 × est)` 10 times → `apply` within 10 %. Also `calibrator_ignores_zero_and_clamps`.
- `prompt_size_counts_cache_for_anthropic_not_openai`.
- `tool_category_covers_every_native_tool` (iterate the registry's names).
- `app`: `live_context_anchors_on_reported_usage`. A fake LLM reports `input 10_000` → next estimate = 10 000 + calibrated delta.
- `app`: `tool_result_telemetry_carries_budget_fields`.
- `app`: `tool_result_meta_carries_subject_and_step`, using a fake tool that returns `subject: Some(FileRange{..})`.
- `infra-telemetry`: `report_contains_ledger_and_context`.
- `infra-session`: `meta_round_trips_and_v1_files_still_load` (a committed v1 fixture).
- `infra-llm` payload snapshots unchanged: **meta is never serialised** (`request_payload_ignores_message_meta`).

## Test-case scenario

`zcode run --json "list the files in crates and read crates/domain/src/lib.rs"` → every `tool_result` JSONL line has `tokens_est`, `chars`, `category`. The report has `ledger.by_category.inspect.tokens_est > 0` and `context.peak_tokens > 0`.

## How to verify

```sh
cargo test -p domain tokens
cargo test -p app live_context
cargo test -p infra-telemetry ledger
cargo test -p infra-llm request_payload_ignores_message_meta
make ci
```

**Pass criteria:** all tests green; `make check-deps` still reports `domain` as one line; request payload snapshots byte-identical to v0.6.0 (no behaviour change); the JSONL schema change is additive only.

## Success metric mapping

Enables M1–M7 measurement (PRD §2.3) and the leading indicators. Prerequisite for the Phase 0 exit gate.
