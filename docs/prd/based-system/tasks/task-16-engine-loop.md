# Task 16 — Engine Orchestration Loop (`App::execute`)

**Related PRD sections:** §3.8 Engine Loop (FR-LOOP-01..04), §3.5 Agent Mode Switching (FR-MODE-01..04), §3.8 FR-TOOL-FS/SHELL dispatch, §3.7 allowlist, §3.2 checkpoint, §3.6 telemetry emission, §5 NFR-REL-03 (crash recovery)
**Depends on:** task-12 (LlmPort), task-15 (ToolRegistry), task-18 (SessionStorePort), task-19 (TelemetryPort), task-20 (Config)
**Status:** To do
**Priority:** High (the central deliverable — the actual agent loop)

## Objective

Turn the v0.1 `App::run`/`App::plan` **stubs** into the real, synchronous **agent loop**: render messages + tool specs to the LLM, stream `LlmEvent`s, dispatch `tool_use`, capture results, truncate oversized results (FR-LOOP-04), checkpoint the session after each round (FR-SESSION-06), emit telemetry events (FR-OUTPUT-*), enforce mode-based tool gating (FR-MODE-01/02) and turn/token caps (FR-LOOP-02/03), and on `Ctrl-C`/timeout flush telemetry + export the partial session (FR-IFACE-05).

## Step-by-step

### 1. `crates/app/Cargo.toml`

`app` stays **domain-only** (FR-DI-02): add `domain` (already) + `thiserror`. **No** tokio/reqwest/serde. The ports are the only dependency surface.

### 2. `crates/app/src/lib.rs` — evolve `App`

Replace the v0.1 `Arc<dyn … + Sync>` fields with owned `Box<dyn … + Send>` and the new port set (§5 of the technical plan; DQ5):

```rust
pub struct App {
    llm: Box<dyn LlmPort + Send>,
    tools: Box<dyn ToolRegistryPort + Send>,
    sessions: Box<dyn SessionStorePort + Send>,
    telemetry: Box<dyn TelemetryPort + Send>,
    logger: Box<dyn LoggerPort + Send>,
    // mode policy (FR-MODE-01): which tool names are execute-side
    execute_only_tools: Box<[String]>,
}
impl App::new(llm, tools, sessions, telemetry, logger, mode) -> Self;
```

`AgentLoop` trait + `ExecutionRequest`/`ExecutionResult` (§5 of the plan) replace `TaskRunner`/`EditPlanner`. The old stubs are removed; update `app`'s unit tests to the new shape (replace the two stub-assertion tests with loop tests using fakes).

### 3. The loop (`AgentLoop::execute`)

```text
fn execute(&mut self, ctx, req) -> Result<ExecutionResult, AppError>:
    session = open_or_create(req.session_id)             // FR-SESSION-02 / create
    history: Vec<LlmMessage> = session.messages            // resumed transcript
    start = Instant::now
    for turn in 0..req.max_turns {
        tools = build_tool_specs(req.mode)                 // FR-MODE-01/02 gating
        req.llm.messages = history + [user prompt w/ images]
        self.telemetry.emit(loop_start)
        finish = None
        // ---- stream ----
        for ev in self.llm.stream(req) {
            match ev:
              Delta(t)      => { self.telemetry.emit(llm_delta); append_text(&mut assistant_msg, t) }
              ToolCallStart{id,name} => start a pending tool_call buffer
              ToolCallArgs{id,args}  => accumulate args JSON
              ToolCallDone           => push ToolCall to assistant_msg.tool_calls
              Finish(f)           => { finish = Some(f); break }
        }
        // ---- decide ----
        match finish.reason:
          Stop/Length => final answer = assistant_msg.content; break
          ToolUse =>
            for each tool_call:
              if req.mode == Planning && is_execute_side(tool_call.name)
                  => refuse (FR-MODE-01): emit error event, push refusal text, AppError::Tool("denied in planning")
              // else dispatch
              result = match tool_call.name:
                  native/tool registry call → ToolRegistry::call
                  mcp::*                  → McpPort
                  lsp::*                  → LspPort
              truncated = truncate(result.content, max_tool_output_chars)  // FR-LOOP-04
              push ToolResult message; push to history
            self.sessions.checkpoint(session.id, &session)  // FR-SESSION-06 every round
        }
    }
    self.telemetry.emit(finish{...})
    self.telemetry.flush_report(session.id, totals)
    Ok(ExecutionResult { final_text, steps, finish_reason, truncated: steps >= max_turns })
```

### 4. Mode gating (FR-MODE-01/02/03/04)

`execute_only_tools` for `Build` = `[write, str_replace_editor, shell, ag:skill? no, lsp::rename_symbol]`; for `Planning` the engine **refuses** any tool whose name starts with `write`/`str_replace_editor`/`shell`/`lsp::rename_symbol` and instead prompts the LLM to confirm. Read-only tools (`read`, `list_dir`, `hover`, `find_references`, MCP reads) remain available in Planning (FR-MODE-01). Mode is stored in session metadata + every telemetry event (FR-MODE-04).

A tiny mode→system-prompt template map lives in `domain::modes` (§FR-MODE-03: "prompt templates + tool restriction" — orchestrate-only, no external files):

```rust
// domain/modes.rs (pure, no deps)
pub fn system_prompt(mode: AgentMode) -> &'static str {
    match mode {
        Planning => "You are a planning agent. Propose edits and ask for confirmation. Do NOT call write/str_replace/shell/rename.",
        Build    => "You are an autonomous coding agent. Make the edits directly.",
    }
}
```

### 5. Turn/token caps + truncation

- `req.max_turns` (default 20) — FR-LOOP-02; exceeding ⇒ `truncated=true`, emit `finish{reason=Length}`.
- `req.max_tokens` forwarded to `LlmRequest` → provider caps output; loop also stops at cap (FR-LOOP-03).
- `max_tool_output_chars` (default 16000) trims each tool result and appends `"...[truncated]"` (FR-LOOP-04).

### 6. Abort / Ctrl-C + timeout

- A `CancelFlag` (shared `Arc<AtomicBool>`) is flipped by the CLI's SIGINT handler (task-17). `execute` checks it at the top of each turn and after each event; if set, it `flush_report` + `checkpoint` the partial session and returns `AppError::Interrupted` (FR-IFACE-05). The CLI maps this to a clean exit (code 130) — **no stack trace** (NFR-REL-01).
- `--timeout <secs>` (FR-IFACE-05) is enforced the same way: `Instant::now()` checked each iteration; on breach, checkpoint + report + `AppError::Timeout`.

### 7. Fakes for tests

Provide `#[cfg(test)]` structs mirroring v0.1's Noop* pattern but richer:
- `FakeLlm`: a scripted iterator of `LlmEvent`s (one `Finish(ToolUse)` carrying a canned `str_replace_editor` tool_call, then next call a `Finish(Stop)`). Deterministic, no network.
- `FakeToolRegistry`: `call` returns a canned `ToolResult` and records the call; `list` returns a single `read` spec.
- `FakeSessionStore`: in-memory `HashMap<String, Session>`, real checkpoint semantics (last write wins).
- `FakeTelemetry`: buffers events in a `Vec` instead of stdout; `flush_report` is a no-op tracking the totals.

### 8. Tests

- `loop_edits_file_via_fake_llm_tool_call`: FakeLlm emits a `str_replace` tool_use; FakeToolRegistry records `call("str_replace_editor", …)`; assert `steps == 1`, `final_text` empty (tool-only turn), `checkpoint` called.
- `planning_mode_refuses_write`: mode=Planning, FakeLlm emits a `write` tool_use → engine returns `AppError::Tool`, telemetry emits a `denied` event, no `call` reaches the registry.
- `build_mode_executes_write`: mode=Build, FakeLlm emits `write` → registry `call` invoked.
- `max_turns_truncates`: FakeLlm always emits `Finish(ToolUse)`; loop runs `max_turns=3` → returns `truncated=true`, `steps==3`.
- `max_tool_output_chars_truncates`: FakeToolRegistry returns 20_000-char content, `max_tool_output_chars=16000` → the pushed ToolResult is trimmed + `"...[truncated]"`.
- `interrupt_between_turns`: CancelFlag set → engine checkpoints + reports + returns `AppError::Interrupted` (FR-IFACE-05).
- `history_appended_across_turns`: two-turn fake (tool_use then stop-on-second) → `history` has user + assistant(tool_calls) + tool + assistant(stop); `final_text` non-empty.

## Test-case scenario

- Headless: `ag run "rename foo to bar in crates/domain/src/model.rs"` → engine streams deltas → emits one `Finish(ToolUse)` with `str_replace_editor` args → `ToolRegistry` applies the edit → second turn `Finish(Stop)` → final text + `.ag/reports/<ts>-<session>.json`.
- Planning: `ag run --mode planning "show me how to rename foo"` → engine refuses `write`/`str_replace` and reports `AppError::Tool("denied in planning")`.
- Ctrl-C mid-loop → partial session checkpointed + report flushed (FR-IFACE-05).

## How to verify

```
cargo test -p app
cargo test -p app -- --ignored            # none expected (all hermetic)
cargo clippy -p app -- -D warnings
cargo tree -p app                        # must show only: domain, thiserror (FR-DI-02)
```

**Pass criteria:** loop dispatches tool_use → tool → result → re-prompt → finish; mode gating rejects execute-side tools in Planning; caps/truncation honored; abort checkpoints + reports cleanly; `cargo tree -p app` = `{domain, thiserror}`; zero `unsafe`.

## Success metric mapping

- M1.5 (end-to-end edit via fake LLM + tool), M1.13 clippy-clean, FR-LOOP-01..04, FR-MODE-01..04, FR-OUTPUT-06/07/08 (steps/time/model in telemetry), FR-SESSION-06 (checkpoint every round), FR-IFACE-05 (abort+flush), NFR-REL-01 (no panic traces), NFR-REL-03 (crash recovery), DQ5 (owned ports), FR-DI-02 (app → domain only).

## Notes / risks

- The loop is **synchronous** by design (DQ4). The TUI (task-17) runs this on a worker `std::thread` and receives `UiEvent`s; headless `ag run` runs inline. Keeping `app` single-threaded and sync is what lets `domain`/`app` stay dep-free.
- `ToolResult` content is capped **after** the tool returns but **before** it enters history, so the LLM never sees >16k chars of tool output in one turn (FR-LOOP-04 memory guard, NFR-PERF-03).
