# Task 23 — Prompt Caching: Breakpoint Layout, Byte-Stable Prefix, Split Accounting, Pricing

**Related PRD sections:** §5.7 FR-CACHE-01..06; §1.1 B6, B7; §2.3 M1, M4, M5
**Technical plan:** CE-DQ6, CE-DQ8, CE-DQ9, CE-DQ10, §5.1, §7.4, §7.6, §10
**Depends on:** task-21 (`cache_tokens()` helper, `PromptSize`)
**Phase:** 1 (v0.7.0)
**Status:** Todo
**Priority:** Critical. The largest single cost lever in the milestone.

## Objective

1. Cache the **conversation** on Anthropic routes, not just the system prompt and tools, with a breakpoint layout that cannot miss the 20-position lookback (CE-DQ6).
2. Guarantee that each request's prefix is byte-identical to the previous one (FR-CACHE-02), and prove it with golden tests.
3. Derive OpenAI-shaped marker use from the model family, and send `prompt_cache_key` to OpenAI (CE-DQ8).
4. Split cache usage into read and write in every layer, and price each correctly (CE-DQ9, FR-CACHE-05). Compute the true prompt size (FR-CACHE-06).

## Step-by-step

### 1. `domain`: split usage (CE-DQ9)

- `LlmFinish`: replace `cache_tokens` with `cache_read_tokens`, `cache_write_tokens`. `cache_tokens()` now returns the sum.
- `TelemetryEvent`, `TelemetryTotals`, `ExecutionResult`: the same split.
- `pricing.rs`:
  - `PriceEntry` gets `cache_write_5m: f64` (default `1.25`) and `cache_write_1h: f64` (default `2.0`), as multipliers of `input_per_mtok`. `cache_per_mtok` is documented as the **read** rate.
  - `estimate(model, input, output, read, write_5m, write_1h) -> Cost`. `Cost` gets `cache_read_usd` and `cache_write_usd` (keep `cache_usd()` as their sum for display).
  - `cache_within_input` semantics unchanged: for OpenAI-family entries, `billable_input = input - read`.
- Fix every compile error across the workspace. The TUI's `Totals` and `/cost` use `cache_tokens()` for now; task-31 renders the split.

### 2. `infra-llm` decoders

- `OpenAiDecoder` usage: `prompt_tokens_details.cached_tokens` → `cache_read_tokens`; `prompt_cache_hit_tokens` (DeepSeek, top-level) → `cache_read_tokens` when the former is absent. OpenAI-shaped writes are not reported → `0`.
- `AnthropicDecoder`: `cache_read_input_tokens` → read, `cache_creation_input_tokens` → write. Stop reading the undocumented `cache_creation_output_tokens`, and fix the fixture at the `message_start` test that includes it. `message_start` usage and `message_delta` usage are merged by **max per field** (the existing hold-back logic keeps the terminal `Finish` until end-of-stream).
- `OllamaDecoder`: no cache fields → 0.

### 3. `infra-llm`: Anthropic layout (CE-DQ6)

In `build_anthropic_request`, after `messages` are rendered:

```rust
fn place_breakpoints(messages: &mut [Value], ttl: &CacheLayout) {
    // BP3: last markable block of the last message.
    let last = messages.len().checked_sub(1);
    // BP4: last markable block of the message *before* the most recent assistant message,
    //      i.e. where the previous request ended. Stateless: each step appends
    //      [assistant, user(tool_results…)].
    let prev_end = messages.iter().rposition(|m| m["role"] == "assistant").and_then(|i| i.checked_sub(1));
    for idx in [prev_end, last].into_iter().flatten().collect::<BTreeSet<_>>() {
        mark_last_markable_block(&mut messages[idx], ttl.tail());
    }
}
```

- Markable block types: `text`, `tool_use`, `tool_result`, `image`. `mark_last_markable_block` walks backwards inside the message. It converts a string `content` into a one-element `[{type:"text", text}]` array before marking.
- System (BP2) and last tool (BP1) keep their current placement, with `ttl.head()`.
- Assert `count_markers(payload) <= 4` with a `debug_assert!`, and test it.
- `CacheLayout` defaults to 5 m everywhere in this task. Task-31 adds the TTL policy.

### 4. `infra-llm`: OpenAI-shaped (CE-DQ8)

- Replace `cache_control: bool` + `with_cache_control` with `markers: CacheMarkers { Auto, On, Off }` (`Auto` default). `fn markers_on(&self) -> bool` implements the family rule on `domain::model_id::normalize(model)`: prefix `claude`, `anthropic/`, `gemini`, `google/`.
- When on: the system message becomes `[{"type":"text","text":…,"cache_control":{"type":"ephemeral"}}]`, and the last message keeps its existing marker. On OpenAI-shaped routes the second rolling breakpoint (BP4) is applied the same way as for Anthropic.
- `prompt_cache_key`: only when `self.provider == "openai"`, set from `LlmPort::set_session(id)` (new trait method with an empty default; `App::execute` calls it once with `session.id` before the loop).
- Remove `.with_cache_control(true)` at the OpenRouter construction site. `Auto` covers it.

### 5. Byte stability (FR-CACHE-02)

- `tools/src/lib.rs`: sort discovered MCP tools by `(server, tool)`. MCP servers register in config order, and config order is part of the user's file, so it is stable. Add `mcp_tools_are_listed_in_sorted_order`.
- Verify `serde_json` has no `preserve_order` feature in the workspace (`cargo tree -e features -i serde_json`). Add a comment in the workspace `Cargo.toml` noting that it must stay off (key order must stay sorted).
- `domain::modes::system_prompt` returns `&'static str`, so it is already stable. Add `system_prompts_contain_no_volatile_data` (no digits that look like a date or time, no `{}` placeholders).
- `app`: the images for the first turn stay attached to the first user message (already the case); test that step 2's request has the same message-0..n bytes as step 1's.

### 6. Correct prompt arithmetic (FR-CACHE-06)

`PromptSize::from_finish` (task-21) now uses the split fields. Test that an Anthropic finish with `input 1_200, read 48_000, write 3_000` gives `52_200`.

### 7. Telemetry and session mirrors

- `infra-telemetry` JSONL: emit `cache_read_tokens`, `cache_write_tokens`, and `cache_tokens` (deprecated, = sum) for one release. Report totals likewise, plus `"cache": {"read", "write", "hit_ratio"}` where `hit_ratio = read / (prompt tokens on steps ≥ 2)`. The per-step prompt size is taken from the `llm_finish` events, which gain `prompt_tokens`.
- `opencode.rs`: `tokens.cache.read` and `tokens.cache.write` get the real split (remove the hardcoded `write: 0` and its comment).
- `infra-session`: `Session` stores no usage, so no change beyond compiling.

### 8. Integration guard (network, `#[ignore]`)

`anthropic_second_identical_request_reads_cache`: two identical requests with a >4 k-token system prompt → second `cache_read_tokens > 0`. Same for OpenAI with a >1,024-token prefix (`openai_second_request_reads_cache`). Needs `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`; skipped silently when absent.

## Tests (hermetic)

- `anthropic_layout_marks_tools_system_prev_end_and_last` (5-step fake session: marker positions at every step).
- `anthropic_layout_never_exceeds_four_markers` (including a first step where `prev_end` is `None`, and a step where `prev_end == last`).
- `anthropic_marker_walks_back_to_markable_block`.
- `prefix_bytes_are_stable_across_steps`: for each of the four builders, render requests 1..5 of a growing fake session, remove every `"cache_control":{…}` with a regex, and assert request *k* serialised is a byte prefix of request *k+1* **in its `messages` array**, and that `tools` and `system` are byte-equal.
- `openai_markers_auto_by_family` (claude → on, gpt-4o → off, gemini → on).
- `openai_sends_prompt_cache_key_only_to_openai`.
- Decoders: `openai_cached_tokens_are_reads`, `deepseek_hit_tokens_are_reads`, `anthropic_split_read_write`.
- Pricing: `writes_cost_1_25x_reads_0_1x`, `one_hour_writes_cost_2x`, `openai_reads_are_not_double_billed`, `provider_reported_cost_still_wins`.
- Telemetry: `opencode_cache_split_is_real`, `report_hit_ratio`.

## Test-case scenario

A 10-step Anthropic session editing a Rust file. The report shows `cache.hit_ratio ≥ 0.75` over steps 2–10. `/cost` (via `cache_tokens()`) shows the cache total. The JSONL `llm_finish` events show `cache_read_tokens` growing step by step, while `cache_write_tokens` stays roughly equal to the previous step's appended content.

## How to verify

```sh
cargo test -p infra-llm cache
cargo test -p infra-llm prefix_bytes_are_stable_across_steps
cargo test -p domain pricing
cargo test -p infra-telemetry
ANTHROPIC_API_KEY=… cargo test -p infra-llm -- --ignored anthropic_second_identical_request_reads_cache
make ci
```

**Pass criteria:** golden tests green; the live test shows reads on the second request; the evaluation run (task-22 harness) shows M4 ≥ 75 % on the Anthropic route and M5 ≥ 50 % on the OpenAI route with no GR regressions; CHANGELOG *Changed* records the JSONL `cache_tokens` deprecation.

## Success metric mapping

M1 (largest share), M4, M5. FR-CACHE-01..06. Resolves B6 and B7.
