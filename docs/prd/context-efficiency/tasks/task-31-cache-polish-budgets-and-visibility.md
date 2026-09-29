# Task 31 — Cache TTL Policy, Cache-Reset Telemetry, Regression Guard; Per-Tool Budgets; `/context`, `/cost`; Search Timeout and Steering

**Related PRD sections:** §5.7 FR-CACHE-07..09; §5.0 FR-BUDGET-05, 06; §5.8 FR-READ-06; §5.1 FR-SEARCH-08, 09
**Technical plan:** CE-DQ7, CE-DQ9, §6.1, §7.4, §7.8, §9
**Depends on:** task-23 (split usage, layout), task-25 (grep), task-26 (shaping), task-28 (`UiEvent::CacheReset`)
**Phase:** 2 (v0.8.0)
**Status:** Todo
**Priority:** Medium. Completes the caching and visibility story; several items are small.

## Objective

1. TTL policy (`5m` / `1h` / `auto`), with a learned fallback when a gateway rejects `ttl` (CE-DQ7).
2. Cache resets recorded for mode, provider/model switches and compaction, and a regression guard in the evaluation harness.
3. Per-tool output budgets replacing the single character cap.
4. `/context` breakdown and `/cost` with cache read/write and savings in the TUI.
5. The `grep` timeout, and telemetry for shell-based search (the leading indicator).

## Step-by-step

### 1. TTL policy (`infra-llm`)

```rust
pub enum TtlPolicy { FiveMin, OneHour, Auto }
pub struct CacheLayout { head: Ttl, tail: Ttl }       // head = BP1/BP2, tail = BP3/BP4
impl CacheLayout { pub fn for_policy(p: TtlPolicy, interactive: bool) -> Self }
// Auto + interactive  → head 1h, tail 5m   (longer TTLs first: allowed ordering)
// Auto + headless     → 5m / 5m
```

- `LlmPort` gets a defaulted `fn set_interactive(&mut self, on: bool) {}`. The TUI's wiring calls it with `true`.
- Marker JSON: `{"type":"ephemeral"}` for 5 m, `{"type":"ephemeral","ttl":"1h"}` for 1 h. Applies to Anthropic and to OpenAI-shaped routes with markers on.
- **Learned fallback:** in `send_with_retry`'s 400 handling, if the body mentions `ttl` (case-insensitive) and the request used a 1 h marker, set an `AtomicBool downgrade_ttl` on the client, rebuild the payload with 5 m, retry once, and emit `LlmEvent::Retry` with the reason "cache ttl not supported; using 5m". The retry is surfaced as a `UiEvent::Retry`, following the existing retry notice path.
- Pricing: the engine passes `write_5m` and `write_1h` to `PriceTable::estimate`. Anthropic's usage breakdown `cache_creation.ephemeral_1h_input_tokens` (read into telemetry in task-23) splits the writes. When absent, all writes are priced at the TTL of the tail breakpoint.
- Config `[cache] ttl = "auto"`, `markers = "auto"` (the latter overrides CE-DQ8's family rule: `on`/`off`).

### 2. Cache resets (FR-CACHE-08)

`UiEvent::CacheReset { reason }` and telemetry `cache_reset {reason}` are emitted by:

- `App::execute`, when the resumed session's stored mode ≠ `req.mode` (`"mode"`).
- `App::set_llm` (the TUI `/provider` and `/model` switches) → sets a flag, and the next `execute` emits `"model"` once.
- Compaction (task-28, already emitted; reason `"compaction"`).

TUI note (dim): `prompt cache reset: mode changed — the next request re-writes the cache`.

### 3. Evaluation regression guard (FR-CACHE-09)

`zcode-evals compare` fails when the hit ratio drops more than 10 pp against the base on any route (exit code 2, and the offending tasks are listed). Document the `#[ignore]` network tests from task-23 in `evals/README.md` as the pre-release check.

### 4. Per-tool budgets (FR-READ-06)

- `domain`: `pub struct ToolBudgets { default_tokens: u32, per_tool: Vec<(String, u32)> }` with `fn for_tool(&self, canonical: &str) -> u32` (prefix match so `mcp__` and `lsp__` families work).
- Defaults (tokens): `shell 6000, read 8000, grep 3000, glob 1500, list_dir 1500, lsp__ 2000, mcp__ 6000, outline 2000, symbols 1500, related 1500`, and `default 6000`.
- `app`: `shape_tool_output(content, chars_for(budget))`, where `chars_for(t) = t × 3.6` (the code divisor from CE-DQ11), and the global `max_tool_output_chars` stays a hard ceiling: `min(chars_for(budget), max_tool_output_chars)`.
- Config `[context.tool_budgets]` table (keys are canonical tool names or `mcp__`/`lsp__` prefixes), merged per key across layers. `zcode config` prints the effective table.

### 5. `/context` (FR-BUDGET-05)

- `cli/src/cli/tui/command.rs`: `SlashCommand::Context`, with help text "what fills the model's context right now".
- The engine thread's `Command` enum gets `Context`. It replies with `EngineMsg::ContextBreakdown(ContextBreakdown)`, computed in `app` by a new `App::context_breakdown(&self, session_id) -> ContextBreakdown`. That function loads the session through the store (the engine is idle between turns, so there is no concurrent mutation) and classifies each message by `meta.kind` and role, plus the tool schema size (`estimate_tokens` of the serialised specs, computed in `tools` and passed in at wiring time as a number).
- Rendering (timeline note, fixed-width table):

```
context  61.2k / 200k (31%)
  system + repo map     2.1k
  tool schemas          3.3k
  summaries             1.8k
  user / assistant      9.4k
  tool results         44.6k   largest: read src/app/lib.rs (s12) 9.8k · shell cargo test (s15) 6.0k · …
  free                138.8k
```

The breakdown's total must be within ±1 % of the engine's `live_tokens`: both use the calibrated estimator, anchored the same way.

### 6. `/cost` (FR-BUDGET-06)

`Totals` (TUI) keeps `cache_read` and `cache_write` separately (from `UiEvent::Usage`, via `set_turn_usage`'s difference logic, extended to both fields). `/cost` adds:

```
cache      read 812.4k · write 64.1k · hit 91%
saved      ≈ $2.19 vs uncached (read billed at 0.1×)
```

`saved = read × (input_rate − read_rate)` from the price table. Shown as `n/a` when unpriced (never a confident `$0.00`, CLAUDE.md "Cost").

### 7. Search timeout and steering (FR-SEARCH-08, 09)

- `GrepTool`: the cancel closure = engine cancel flag OR `started.elapsed() > search.timeout_ms`. A partial result adds `partial: timed out after 10s — narrow with path/glob/type` to the footer.
- Shell search detection needs the command parsed out of the JSON arguments, which `app` cannot do (no serde). Add `ToolRegistryPort::classify_call(&self, name: &str, args_json: &str) -> Option<&'static str>` with a default of `None`. `tools` implements it: for `shell`, it takes the command, skips leading `cd … &&` and `env VAR=… ` prefixes, and returns `Some("shell_search")` when the first word is one of `grep|rg|ag|ack|find|fd`. The engine adds the returned label to the `tool_call` telemetry extras, and the report ledger counts `shell_search_calls`.
- `auto` system prompt: a single fixed sentence already added in task-25. No change here.

## Tests

- `ttl_auto_interactive_puts_1h_on_head_5m_on_tail`; `ttl_order_is_long_before_short`; `ttl_rejection_downgrades_once_and_retries` (canned 400 body mentioning `ttl`).
- `pricing_splits_1h_and_5m_writes`.
- `cache_reset_emitted_on_mode_change_and_model_switch`.
- `evals_compare_fails_on_hit_ratio_drop`.
- `tool_budgets_prefix_match_and_ceiling`; `config_tool_budgets_merge_per_key`.
- `context_breakdown_sums_to_live_estimate` (±1 %).
- `slash_context_parses` (command.rs tests); TUI snapshot of the rendered breakdown.
- `cost_shows_cache_split_and_saving`; `cost_saving_is_na_when_unpriced`.
- `grep_times_out_with_partial_footer` (a fake SearchPort that sleeps).
- `shell_search_calls_are_counted`.

## Test-case scenario

A TUI session with pauses of 10–40 minutes between prompts: with `auto`, BP1/BP2 still read from cache after the pause (1 h), while the conversation tail rewrites once. `/cost` shows a hit ratio above 80 %. `/context` explains why the context is 61 k: one 9.8 k read from step 12 dominates.

## How to verify

```sh
cargo test -p infra-llm ttl
cargo test -p app budgets context_breakdown
cargo test -p zcode slash tui cost
cargo test -p zcode-evals
make ci
```

**Pass criteria:** tests green; the Phase 2 evaluation shows M4/M5 holding or improving against Phase 1, and GR3 unchanged.

## Success metric mapping

M1, M4, M5; leading indicator (shell search share); US-06, US-08. FR-CACHE-07..09, FR-BUDGET-05..06, FR-READ-06, FR-SEARCH-08..09.
