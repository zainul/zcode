# Task 17 — CLI: Headless `run` + Interactive TUI + Session/Tool/Skill Subcommands

**Related PRD sections:** §3.1 Multi-Interface (FR-IFACE-01..06), §3.9 CLI Command Matrix, §3.7 skills, §3.8 caps, §5 NFR-PERF-01 (cold <300 ms), §7 M1.5/M1.6/M1.8/M1.11, §8 DQ4 (current-thread) / DQ8 (ratatui)
**Depends on:** task-16 (App::execute), task-20 (Config), task-18 (SessionStorePort), task-19 (TelemetryPort), task-15 (ToolRegistry)
**Status:** To do
**Priority:** High (the user-facing surface; the two interfaces share one engine per FR-IFACE-03)

## Objective

Extend the v0.1 `ag version`-only CLI into the full command matrix while preserving the shared engine (FR-IFACE-03). Two interfaces share `App::execute`:

1. **Headless** — `ag run "<prompt>"` runs one agent turn and exits; with `--json` it streams one JSON object per event (FR-IFACE-04, FR-OUTPUT-01). `ag version`, `ag session …`, `ag tools list`, `ag skills list` are also headless.
2. **Interactive TUI** — `ag` / `ag repl` launches a `ratatui` screen with message + tool pane; the blocking engine runs on a **dedicated `std::thread`** (keep current-thread runtime free to render), streaming `UiEvent`s back via `mpsc`. `q`/`Ctrl-C` aborts, flushes telemetry + partial session (FR-IFACE-05).

The CLI is the composition root: `wire(&Config) -> Result<App, AppError>` constructs the matching provider LLM (FR-MODEL-06), the `ToolRegistry` (native + configured MCP + optional LSP), the session store, and telemetry (FR-IFACE-03 shared).

## Step-by-step

### 1. `crates/cli/Cargo.toml`

```toml
[dependencies]
domain; app; infra-llm; infra-filesystem; infra-shell; infra-config; infra-mcp; infra-lsp;
infra-session; infra-telemetry; crates/tools  (path deps)
clap = { workspace = true }
tokio = { workspace = true, features = ["rt", "macros", "time"] }   # current_thread; time for timeout poll
ratatui = { workspace = true }
crossterm = { workspace = true }
uuid = { workspace = true }            # only for CLI session-id formatting if needed
log = { workspace = true }
env_logger = { workspace = true }

[features]
default = []
pty = ["infra-shell/pty"]               # future persistent shell (task-21)
```
`tokio` gains only `rt`, `macros`, `time` (no `rt-multi-thread` — single-threaded per §8 assumption). `#![forbid(unsafe_code)]` stays (NFR-PORT-02).

### 2. `crates/cli/src/cli/mod.rs` — clap + subcommands

```rust
#[derive(Parser)]
#[command(name="ag", version, about="QAgent — the lean Rust coding agent")]
pub struct Cli { #[command(subcommand)] pub command: Commands }

pub enum Commands {
    Version,
    Run(RunArgs),
    Repl(ReplArgs),
    Session(SessionCmd),
    Tools { list: bool },
    Skills { list: bool },
}
pub struct RunArgs {
    prompt: String,
    images: Vec<PathBuf>,        // --image foo.png  (FR-IFACE-01)
    mode: AgentMode,            // --mode planning|build
    session: Option<String>,
    json: bool,                 // --json  (FR-IFACE-04)
    config: Option<PathBuf>,     // --config <path>
    timeout: Option<u64>,       // --timeout <secs>  (FR-IFACE-05)
}
```

### 3. `wire(&Config) -> Result<App, AppError>` (composition root)

```rust
pub fn wire(cfg: &Config) -> Result<App, AppError> {
    let api_key = cfg.resolve_api_key()?;                       // FR-CONFIG-03, fail-fast
    let llm = match cfg.provider {
        Openai => OpenAiLlm::new("https://api.openai.com/v1/chat/completions", &api_key, &cfg.model),
        Anthropic => AnthropicLlm::new(...),                    // FR-MODEL-01..04
        Openrouter => OpenRouterLlm::new(...),
        Ollama => OllamaLlm::new("http://localhost:11434/api/chat", &cfg.model),
        Vllm | OpenaiCompatible => OpenAiLlm::new(&cfg.base_url.unwrap(...), &api_key, &cfg.model),
        unknown => return Err(AppError::Config(...)),          // FR-MODEL-06 typed error
    };
    let tools = ToolRegistry::new(cfg);                        // native + mcp + lsp (FR-MCP-03/04)
    let sessions = UuidSessionStore::new(cfg.working_dir.join(".ag").join("sessions"));
    let telemetry = JsonTelemetry::new(stdout_or_sink(cfg.json), cfg.working_dir.join(".ag").join("reports"));
    Ok(App::new(Box::new(llm), Box::new(tools), Box::new(sessions), Box::new(telemetry), ...))
}
```
`AppError` gains `Config(String)` variant (FR-MODEL-06: unknown provider → typed error, not panic — NFR-REL-02). Each MCP server that fails to start is logged+skipped (FR-MCP-05) — `ToolRegistry::new` swallows `McpClient::new` errors.

### 4. Headless `ag run` (FR-IFACE-01/03/04)

```rust
Commands::Run(a) => {
    let cfg = Loader::with_default().load_with_override(a.config)?;
    let app = wire(&cfg)?;
    let req = ExecutionRequest { prompt, images, mode: a.mode, session_id: a.session,
        max_turns: cfg.max_turns, max_tokens: cfg.max_tokens, max_tool_output_chars: cfg.max_tool_output_chars, ... };
    if a.json { app.execute(&ctx, req) }          // JsonTelemetry emits JSONL to stdout
    else { app.execute_streaming(&ctx, req, &mut line_by_line_printer) }   // pretty terminal output
}
```
`--json`: the engine emits JSONL; we just don't print a second pretty layer. `Ctrl-C`/timeout: SIGINT handler flips the shared `CancelFlag`, the loop checkpoints+reports, exit 130 (FR-IFACE-05).

### 5. TUI — `crates/cli/src/cli/tui.rs` (new)

ratatui layout: `chunks = Layout::default().direction(Vertical).constraints([30%, 60%, 10%])` → messages (top), tool calls/results (middle), input bar (bottom).

```rust
pub fn run_tui(cfg: Config) -> Result<(), AppError> {
    let (tx, rx) = mpsc::channel::<UiEvent>();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel2 = cancel.clone();
    // worker thread: run the blocking engine, send UiEvents
    let handle = std::thread::spawn(move || {
        let mut app = wire(&cfg)?;
        let req = ExecutionRequest::from_repl(...);
        app.execute_with_emitter(&ctx, req, |ev| { let _ = tx.send(ev); }, &cancel2);
    });
    // current-thread tokio powers the renderer only
    ratatui renderer: crossterm backend, event::read() poll loop;
      on q / Ctrl-C / Esc => cancel.store(true); break;
    drain channel → push to ui buffers; re-render each tick.
    join worker → return.
}
```
`execute_with_emitter` is an `app` method variant that takes a callback instead of writing to a `Box<dyn Write>` — keeps `app` dep-free while letting the TUI inject rendering. (The callback signature returns nothing and is `FnMut(UiEvent)`; app holds no tokio, the TUI does.)

### 6. Remaining subcommands

- `ag version` — unchanged (FR-IFACE-06).
- `ag session create|continue|cork|import|export` — delegate to `UuidSessionStore` (FR-SESSION-01..05).
- `ag tools list` — `wire` the registry, print `ToolSpec` names (native + `mcp::*` + `lsp::*`).
- `ag skills list` — `fs::read_dir(skills_dir)`, print `*.md` names (FR-OUTPUT-09).

### 7. Ctrl-C / timeout handling

A `ctrlc`-style handler is **not** a crate we add lightly; instead, for headless, run the engine and install a SIGINT handler via the standard `signal_hook` minimal crate OR — simpler, dep-light — rely on the user sending SIGINT and catching `Err(AppError::Interrupted)` when the loop's `CancelFlag` is set. To set the flag from a signal handler with **zero extra crates**: spawn a `tokio::task` (current_thread) that `signal_recv` style isn't available in std. 

**Decision (DQ12):** add `signal-hook` (tiny, no unsafe to consumers) as the only signal crate. `wire` registers a handler that sets the shared `Arc<AtomicBool>`; the engine polls it. `--timeout` is the loop's own `Instant` check (no signal needed). This keeps signal handling in the CLI composition root (interface), not in `app`/`domain` (preserves FR-DI-01/02).

`signal-hook` crate:
```toml
signal-hook = "0.3"   # registers a Unix signal handler setting the flag
```
(Windows graceful-abort relies on Ctrl-C → default process termination; PTY Windows deferred to v0.2.1 per PRD §6 #11.)

### 8. Tests

- `wire_dispatches_provider`: `Config{provider: Anthropic, ...}` → `App` constructed with an `AnthropicLlm` (assert via a `cfg`-gated `App::kind()` debug accessor or by exercising behavior — keep an internal `pub(crate)` probe). At minimum, `wire` returns `Ok` for each known provider and `Err(AppError::Config)` for `"bogus"`.
- `run_subcommand_parses_all_flags`: `Cli::try_parse_from(["ag","run","x","--mode", "planning","--json","--timeout","10"])` → `Commands::Run` with the right fields (clap).
- `version_parses`: unchanged from v0.1 (keep the existing test).
- `ag_run_json_is_jsonl`: `#[ignore]` integration against local Ollama — stdout lines each parse with `serde_json`.
- `tui_launches_and_quits`: manual smoke (`ag repl`, `q` exits 0) — documented as M1.11.

Hermetic: all `wire`/clap tests use `tempfile` configs + a dummy provider (`Provider::Ollama` with a fake local endpoint is still network; so provider-dispatch tests assert only construction, not network). Network tests `#[ignore]`'d.

## Test-case scenario

- TUI: `ag repl` renders; user pastes "rename foo to bar in model.rs"; engine emits deltas into the message pane; on finish writes the file; `q` exits 0 + report flushed.
- Headless: `ag run --json "ls crates"` (allowlisted shell) → stdout lines: `loop_start`, several `llm_delta`, one or more `tool_call`/`tool_result`, `finish` — all `jq -e .`-parseable (M1.6).
- `ag session create` → UUIDv7; `ag tools list` → `read write str_replace_editor list_dir shell ag:skill mcp::* lsp::*`.
- `Ctrl-C` during `ag run` → partial session checkpointed + `.ag/reports/*` written, exit 130, no panic (FR-IFACE-05, NFR-REL-01).

## How to verify

```
cargo test -p ag
cargo run -q -- version                       # M1.6 build meta; T20
cargo run -q -- run "echo hi"                  # FR-TOOL-SHELL allowlisted
cargo run -q -- run --json "echo hi" | jq -e .   # NFR-OBS-01 / M1.6
cargo test -p ag -- --ignored                 # integration (needs local Ollama/MCP)
cargo clippy -p ag -- -D warnings
cargo tree -p ag                             # all layers reachable
du -h target/release/ag                       # M2.4 < 12MB
time ./target/release/ag version              # M2.1 < 300ms
```

**Pass criteria:** `ag version` unchanged; `ag run` runs the loop end-to-end (fake/local); `ag repl` renders + quits on `q`; JSONL is line-valid JSON (M1.6); reports/schema present (M1.7); signal/timeout path checkpoints + reports without panic (FR-IFACE-05, NFR-REL-01); `cargo tree -p ag` reaches all layers (M1.4); release binary < 12 MB (M2.4); cold `ag version` < 300 ms (M2.1); `#![forbid(unsafe_code)]` holds.

## Success metric mapping

- M1.1/M1.3 (build + clippy), M1.5 (e2e edit), M1.6 (JSONL), M1.7 (report schema via task-19), M1.8 (session CLI), M1.11 (TUI smoke), M2.1 (cold <300 ms), M2.4 (<12 MB), FR-IFACE-01..06, FR-MODEL-06 (dispatch), FR-SESSION-01..05 (CLI), FR-OUTPUT-01/09, NFR-SEC-02 (allowlist enforced via task-15), NFR-PERF-01, NFR-PORT-02, DQ4/DQ8/DQ12.

## Notes / risks

- The **only** new runtime dep in `cli` is `ratatui` + `crossterm` (TUI) + `signal-hook` (graceful SIGINT). `app`/`domain`/infra stay lean; `app` gains zero new deps (FR-DI-02).
- `execute_with_emitter` (the TUI variant) is added to `app` in task-16 via a small `Emitter` callback trait defined in domain (pure) — keeps `app`→`domain` only while letting the TUI supply rendering. Document that as a tiny addition to the app/lib API.
- Release binary size guard (M2.4 < 12 MB) is met by `lto="thin"`+`codegen-units=1` (already in profile) + minimal dep set; `ratatui` is the largest addition (~2–3 MB). Monitor at `ci`/release.
