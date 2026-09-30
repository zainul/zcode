# Task 28 — ContextManager: Trigger, Hysteresis, Tier 1 (Supersession), Tier 2 (Elision), Reactive Compaction

**Related PRD sections:** §5.3 FR-CTX-01..06, 09, 10; §1.1 B4, B5; §2.3 M6, M7; §12 R3
**Technical plan:** CE-DQ3, CE-DQ4, CE-DQ10, CE-DQ12, §5.3, §6.1, §6.2
**Depends on:** task-21 (meta, calibrated live size), task-26 (spill; the stubs point at spill files), task-27 (`FileWrite` subjects)
**Phase:** 2 (v0.8.0)
**Status:** Done (v0.7.0)
**Priority:** Critical. Bounds the transcript, and removes the quadratic growth and the context-length failures.

## Objective

Build the deterministic part of compaction: the pure policy in `domain::context` and the orchestration in `app::context::ContextManager`, wired into `AgentLoop::execute`. **No LLM call yet.** Tier 3 (summarisation) comes in task-29, and this task leaves a clearly marked seam for it. On many real sessions, Tiers 1+2 alone reach the target.

## Step-by-step

### 1. `crates/domain/src/context.rs` (new, pure)

```rust
pub struct Policy { pub keep_recent_steps: u32, pub elide_over_tokens: u32 /* default 400 */ }

/// FR-CTX-03. true = must not change.
pub fn protected(h: &[LlmMessage], p: &Policy) -> Vec<bool> {
    // [0] system; first User message; last User message; RepoMap/Summary kinds are *not* protected
    // (a later summary may absorb an earlier one); every message whose meta.step > max_step - keep_recent_steps.
}

/// FR-CTX-05. Indices of tool-result messages made obsolete by a later message (rules in plan §5.3).
pub fn superseded(h: &[LlmMessage]) -> Vec<usize>;

/// FR-CTX-06. Unprotected tool results with meta.tokens_est > elide_over_tokens, oldest first,
/// plus assistant messages whose tool-call *arguments* are large edit payloads (write/apply_patch/str_replace/edit_symbol).
pub fn elidable(h: &[LlmMessage], prot: &[bool], p: &Policy) -> Vec<Elidable>;
pub enum Elidable { Result(usize), CallArgs { msg: usize, call: usize } }

pub fn stub_for_result(msg: &LlmMessage, call: Option<&LlmToolCall>, reason: StubReason) -> String;
pub fn stub_for_call_args(call: &LlmToolCall, subject: Option<&Subject>) -> String;

/// FR-CTX-04. Every tool_call id has exactly one later tool result and vice versa; roles alternate as providers require.
pub fn validate(h: &[LlmMessage]) -> Result<(), ContextError>;

/// Maps each tool-result message to the assistant message + call that produced it.
pub fn pair_index(h: &[LlmMessage]) -> Vec<Option<(usize, usize)>>;
```

Stub text (byte-stable, no timestamps):

- Superseded read: `[superseded — src/lib.rs lines 1-400 was read again at step 12]`
- Superseded by a write: `[superseded — src/lib.rs was modified at step 14; read it again if needed]`
- Elided result: `[elided — read src/lib.rs lines 1-400 at step 7, ~9.8k tokens; full text: .zcode/spill/<sid>/<call>.txt]`. Without a spill path: `…; re-run the call to see it again`.
- Elided call args: `{"path":"src/x.rs","content":"[312 lines elided — written at step 9]"}`. The **arguments stay valid JSON** (some providers re-validate tool_use input), and only large string values are replaced. Keep `path`-like keys. Because `domain` cannot parse JSON, the elision is **done in `app`** through a helper port. Rather than add a JSON dependency, `Tool` gets an optional method `fn elide_args(&self, args_json: &str) -> Option<String> { None }` on `ToolRegistryPort` (default `None`), and `tools` implements it with `serde_json`. When it returns `None`, the call args are left alone.

Why the args are elided at all: a 300-line `write` payload is re-sent on every later step, and it is often the largest message in a session.

### 2. When elided content needs a spill

Tier 2 prefers messages that already have `meta.spill`. For large results that were **not** spilled (they were under budget), `ContextManager` spills them at compaction time through `SpillPort` before stubbing, so every elided result stays recoverable. If there is no spill store, the stub says "re-run the call".

### 3. `crates/app/src/context.rs` (new)

Implement `ContextConfig`, `ContextManager` and `maybe_compact` / `force_compact` per technical plan §6.2, with Tier 3 as:

```rust
// Tier 3 seam — task-29 implements summarise(); until then this is None.
if let Some(summariser) = deps.summariser.as_mut() { … }
```

- `live_tokens` moves here from task-21's private fn (same formula).
- **Hysteresis:** compact only when `live ≥ limit`; stop each tier as soon as `live ≤ target`. `compact_at = 0.75`, `compact_target = 0.45`, `compact_at_tokens = 96_000` for an unknown window (`WindowTable::lookup` returns `None`), with the target scaled the same way.
- Messages changed by compaction have their **old** version collected in `archived: Vec<LlmMessage>`. `SessionStorePort::archive` (default no-op until task-29) is called with them.
- After any change: `domain::context::validate(&h)`. On `Err`: restore the pre-compaction history (clone taken at entry, only when compaction actually runs), emit `UiEvent::Notice("compaction skipped: …")` and a `context_compaction_failed` telemetry event, and continue. Compaction can never corrupt a transcript (FR-CTX-04, FR-CTX-09).
- Record `CompactionRecord { step, tier, tokens_before, tokens_after, archived_from, archived_to }` in `session.compactions` (the domain field is added now; persisting it is task-29).
- Emit `UiEvent::Compacted { tier, tokens_before, tokens_after }` and `UiEvent::CacheReset { reason: "compaction" }` (the new `UiEvent` variants; the TUI and emitters render them generically in this task and polish them in task-29).
- **The emergency path** (FR-CTX-09): if after Tier 2 (and Tier 3 when present) `live > window * 0.95`, elide inside the protected window except the latest step, and the last user message is never touched.

### 4. Wiring in `AgentLoop::execute`

Just before `LlmRequest` is built:

```rust
let window = self.context_window.lookup(&session.model);
if let Some(rec) = self.ctx.maybe_compact(&mut history, &mut session, window, &mut deps)? { compactions += 1; }
```

The live size after compaction feeds `clamp` (task-21 already uses `live_tokens`).

### 5. Reactive compaction (FR-CTX-10)

The provider error path already calls `parse_window_from_error` → `WindowTable::learn`. Extend it: on a context-length error **within a step that has not retried yet**, call `self.ctx.force_compact(...)` (which ignores the `compact_at` threshold and compacts to the target of the *learned* window), then `continue` the loop without incrementing `steps`. A second context-length error in the same step surfaces exactly as today. Test with a fake LLM that returns the OpenRouter 400 text once.

### 6. Config

`infra-config` `[context]`: `compaction` (bool), `compact_at`, `compact_target`, `compact_at_tokens`, `keep_recent_steps`, validated as in technical plan §7.8. `cli::wire` → `App::set_context_config`. `--no-compact` sets `compaction = false` **and** disables reactive compaction (PRD FR-CTX-12 wording). With `compaction = false` alone, reactive compaction stays on.

## Tests

`domain::context` (pure, many cases):
- `protected_includes_system_first_and_last_user_and_recent_steps`.
- Supersession rules: `wider_later_read_supersedes`, `narrower_later_read_does_not`, `write_supersedes_earlier_reads_of_that_path`, `later_diagnostics_supersede`, `identical_search_supersedes`.
- `elidable_is_oldest_first_and_skips_protected`; `edit_call_args_are_elidable`.
- `stub_text_is_deterministic` (snapshot).
- `validate_detects_orphaned_call_and_result`; `validate_accepts_parallel_tool_results`.
- **Property test** (seeded xorshift, 2,000 random transcripts of 5–120 steps with random tool mixes): after `superseded` + `elidable` are applied as stubs, `validate` is `Ok`, and protected messages are byte-identical.

`app::context`:
- `does_not_compact_below_threshold`; `compacts_to_target_with_hysteresis`.
- `tier1_alone_reaches_target_when_enough_is_superseded`.
- `tier2_spills_unspilled_results_before_eliding` (fake SpillPort records the calls).
- `failed_validation_restores_history_and_continues`.
- `unknown_window_uses_absolute_threshold`.
- `reactive_compaction_retries_exactly_once` (fake LLM: 400 context-length, then success); `second_rejection_surfaces`.
- `no_history_bytes_change_between_compactions` (FR-CTX-05 cache stability: snapshot the serialised history before each request; bytes before index *k* are equal across steps unless a compaction happened).
- `compaction_emits_compacted_and_cache_reset`.
- Bench: tiers 1+2 on a synthetic 150 k-token transcript ≤ 50 ms (NFR-CTX-PERF-04), criterion in `benches/`.

## Test-case scenario

A 90-step refactor on the Go fixture repo with `claude-*` (200 k window). v0.6: the context grows past 150 k by step 60. Now: a Tier 1+2 compaction at ~150 k brings it to ~90 k (45 %), and a second at step ~85. No context-length errors. The cache-reset notes show 2 resets in 90 steps.

## How to verify

```sh
cargo test -p domain context
cargo test -p app context
cargo bench -p zcode-benches compaction
make ci
make eval-tokens LIVE=1 LABEL=p2-tiers12   # long-horizon category
```

**Pass criteria:** tests green, including the 2,000-case property test; M6 = 0 and M7 ≤ 80 % on the long-horizon category; long-horizon success ≥ baseline (GR1); ≤ 1 compaction per 25 steps (FR-CTX-02).

## Success metric mapping

M2, M6, M7, GR1. FR-CTX-01..06, 09, 10. Resolves B4 and B5.
