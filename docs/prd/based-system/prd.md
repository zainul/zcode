# PRD: Minimal OpenCode-Capable Agent — QAgent (`ag`) v0.2.0

**Document ID:** PRD-AGENT-CORE-002
**Status:** Draft
**Author:** Technical Product Manager
**Created:** 2026-08-24
**Target Release:** v0.2.0 (Core capability milestone)
**Depends on:** PRD-SCAFFOLD-001 (v0.1.0 foundation) — `docs/prd/initial-scaffolding/prd.md`
**Owner:** Engineering Team

---

## 1. Overview and Goals

### 1.1 Overview

The AI Coding Agent (`ag`) is a terminal-based, AI-driven coding assistant written in Rust that mirrors **1:1 the core capabilities of OpenCode** while deliberately reducing memory footprint and maximizing runtime performance. The v0.1.0 milestone delivered a clean-architecture Cargo workspace with port/adapter traits and stubbed infrastructure. This document defines **v0.2.0**, the milestone that turns the stubs into a **minimal but functional** coding agent.

Concretely, v0.2.0 wires together the five subsystems the user identified as "minimal features like opencode":

1. **Multi-Interface Architecture** — a Terminal User Interface (TUI) for interactive use **and** a Headless CLI for single-run, non-interactive task execution (e.g. `ag run "refactor foo()"`).
2. **Session & State Management** — sessions are created, forked, continued, imported, and exported as portable local artifacts (no cloud lock-in).
3. **Extensible Tool System** — native fast file/shell operations as first-class tools, plus **MCP** (Model Context Protocol) and **LSP** integration so the agent can reach external data sources and reason about code semantically.
4. **Model & Provider Agnosticism** — abstracted provider clients for OpenRouter, OpenAI, Anthropic, and local models (Ollama/vLLM), with swappable **agent modes** (Planning vs. Build).
5. **Multi-modal LLM runtime with telemetry** — JSON-based configuration, an allowed-command allowlist, machine-readable JSON output, and first-class telemetry collecting input/output/cache tokens, execution steps, execution time, and model name per task.

This milestone delivers the **smallest coherent subset** that constitutes a usable coding agent: an end-to-end loop that reads the user's intent from config, asks an LLM, calls tools (file/shell/MCP/LSP), collects tool results, iterates, and emits structured telemetry — usable in both interactive (TUI) and one-shot (headless) modes.

### 1.2 Goals (Why we are doing this)

| # | Goal | Rationale |
|---|------|-----------|
| G1 | Ship a **working end-to-end agent loop** from a single prompt to a completed file edit via LLM tool-calling. | Users must be able to do `ag run "rename variable x to ctx in src/foo.rs"` and observe a real edit. A stub-only release cannot validate the architecture. |
| G2 | Deliver **two interfaces** — interactive TUI and headless single-run CLI — sharing one engine. | OpenCode's value proposition is interactive REPL *and* one-shot scripting; both are table-stakes. |
| G3 | Implement **MCP** and **LSP** as pluggable tool backends so the agent can reach external data and semantic code intel. | Mirrors OpenCode's tool-extensibility; without this the agent is a closed box. |
| G4 | Abstract the LLM layer with **multi-provider** (OpenAI, Anthropic, OpenRouter, Ollama/vLLM) dispatch driven by config. | Single-provider lock-in kills adoption; config-driven provider + model is a hard requirement. |
| G5 | Add **agent-mode switching** (Planning / Build) via prompt templates + tool restriction. | Different tasks need different strategies; OpenCode's "agent modes" are a core differentiator. |
| G6 | Emit **structured JSON output** and collect **telemetry** (tokens, steps, time, model) per run. | Operators need observability + cost attribution; JSON output enables programmatic consumption (CICD). |
| G7 | Enforce **command allowlist** from config for every shell exec. | A coding agent that runs arbitrary shell commands is a supply-chain bomb; the allowlist is the safety boundary. |
| G8 | Keep the **memory/performance envelope** set in v0.1.0 intact under load. | Lean is the product's differentiator vs. the JS OpenCode baseline; regressions are regressions. |

### 1.3 Vision Alignment

> **Build a terminal coding agent in Rust, 1:1 capable of OpenCode's core features (natural language task execution, file editing, shell execution, plugin/MCP/LSP extensibility), while being significantly leaner on memory and faster on cold start.**

v0.2.0 delivers the first release where that claim is empirically true end-to-end. v0.1.0 is the skeleton; v0.2.0 adds the flesh (LLM wiring, tool loop, sessions, telemetry, dual interfaces).

---

## 2. User Stories

### 2.1 End-User Stories (MVP scope for v0.2.0)

| ID | As a… | I want to… | So that… |
|----|-------|------------|----------|
| US-E-01 | Scripter | run `ag run "add a match arm for Result::Err to Foo::bar"` | the agent reads/writes files and edits code via an LLM without me typing in a REPL. |
| US-E-02 | Developer | run `ag` with no args to drop into the interactive TUI | I can iterate on multi-step tasks conversationally, see tool output inline, and abort cleanly. |
| US-E-03 | Developer | have the agent call native file tools (read/write/edit) and `shell:` tools | fast edits and command execution happen without shelling out to `sed`/`grep`. |
| US-E-04 | Developer | connect an MCP server (e.g. a Notion or Postgres MCP) | the agent can query external data sources the way OpenCode does. |
| US-E-05 | Developer | have the agent use LSP go-to-definition / find-references | it edits the *right* symbol and doesn't rename unrelated ones. |
| US-E-06 | Developer | set my provider + model in `ag.toml` (`openai`, `anthropic`, `openrouter`, `ollama`) | I'm not locked into one vendor and can use my local model. |
| US-E-07 | Developer | run in `--mode planning` vs `--mode build` | planning mode asks for confirmation; build mode executes aggressively. |
| US-E-08 | Operator | emit `--json` output and a telemetry file (`ag.report.json`) with token/step/time/model | I can pipe results into CICD and attribute costs. |
| US-E-09 | Developer | create/continue/fork/export a session, save to a local `.ag/sessions/<id>.json` | I can resume long work, branch exploration, and hand sessions to teammates. |
| US-E-10 | Security-conscious user | have a `shell.allowed` list in `ag.toml`; disallowed commands fail | the agent cannot run arbitrary `rm -rf` / exfil commands. |
| US-E-11 | Multimodal user | pass an image (`--image foo.png`) to an LLM with vision (gpt-4o, claude-3-opus) | the agent can reason over screenshots / diagrams. |

### 2.2 Contributor-Facing Stories (enabling)

| ID | As a… | I want to… | So that… |
|----|-------|------------|----------|
| US-C-01 | Engineer | add a new built-in tool by implementing `Tool` trait in `crates/tools` | the agent loop picks it up without touching the engine. |
| US-C-02 | Engineer | add an MCP client adapter by implementing `McpPort` in `infra/mcp` | new protocol backends are drop-in. |
| US-E-03 (cont.) | Engineer | keep `domain` dependency-free while infra crates add `reqwest`/`serde`/`mcp`/`lsp` | architecture boundaries hold under real I/O pressure. |

---

## 3. Functional Requirements

### 3.1 Multi-Interface Architecture

The **engine** (`crates/app`) is interface-agnostic. The **Interface** layer owns two entry points that both drive the same `App::run_task`.

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-IFACE-01 | Headless CLI single-run command | `ag run "<prompt>" [--image <file>]... [--mode <planning\|build>] [--session <id>] [--json] [--config <path>]` executes one agent turn-loop and exits. |
| FR-IFACE-02 | Interactive TUI | `ag` (no `run`, or `ag repl`) launches a `ratatui`-based TUI with message history, streaming tool output, and `q`/Ctrl-C to abort. |
| FR-IFACE-03 | Shared engine | Both interfaces call `App::execute(ctx, plan) -> ExecutionResult` — no duplicated orchestration logic. |
| FR-IFACE-04 | Streaming token UI | TUI streams LLM deltas and tool results live; headless `--json` streams JSON tool-call + result objects line-by-line. |
| FR-IFACE-05 | Graceful abort | `Ctrl-C` (SIGINT) and a `--timeout <secs>` cap terminate the loop, flush telemetry, and export the partial session. |
| FR-IFACE-06 | `version` subcommand preserved | `ag version` continues to print build metadata (unchanged from v0.1.0). |

### 3.2 Session & State Management

Sessions are the durable state unit. On disk layout: `.ag/sessions/<id>.json`. Portable, human-readable JSON so sessions are importable/exportable by hand.

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-SESSION-01 | Create session | `ag session create` allocates an ID (UUIDv7), writes an empty session file, returns the ID. |
| FR-SESSION-02 | Continue session | `ag session continue <id>` loads the session, appends to message history, runs the loop. |
| FR-SESSION-03 | Fork session | `ag session fork <id> --as <new_id>` snapshots the message history into a new ID; the child is independent. |
| FR-SESSION-04 | Import session | `ag session import <file.json>` reads a JSON session (from clipboard, another machine, another agent) into a new local ID. |
| FR-SESSION-05 | Export session | `ag session export <id> --to <file.json>` writes that session's full transcript + telemetry as JSON. |
| FR-SESSION-06 | Auto-checkpoint | Every completed step writes a checkpoint to the session file so a crash mid-run resumes from the last good state. |
| FR-SESSION-07 | Session metadata | Each session records: created_at, model, mode, last_message_at, step_count. |

### 3.3 Extensible Tool System

A `Tool` is any callable that the agent can dispatch via an LLM `tool_use` / `function_call`. The engine has a **tool registry** (`ToolRegistry`) that merges built-in + MCP + LSP tools into one namespace presented to the LLM.

#### 3.3.1 Native File & Shell Operations (built-in tools)

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-TOOL-FS-01 | `read(path)` | Built-in tool reading a file via `infra/filesystem`; returns contents as a tool result string. |
| FR-TOOL-FS-02 | `write(path, content)` | Built-in tool writing a file (atomic: write-temp + rename). |
| FR-TOOL-FS-03 | `str_replace_editor` family | `view`, `create`, `str_replace`, `list_dir` — OpenCode-style string-diff edits, implemented in-process (no shell `sed`). |
| FR-TOOL-SHELL-01 | `shell(command, cwd?, timeout?)` | Built-in tool runs a command **only if every token is allowlisted** (see §3.7). Returns stdout/stderr/exit. |
| FR-TOOL-SHELL-02 | Persistent shell (TUI) | In TUI, `shell` opens a stateful PTY-like pane (spawned `sh` session) for multi-command workflows. *(Stubbed in headless: `spawn()` was deferred to v0.2; now implemented for PTY use via `repro-get`/`tokio` + `pTY` crate behind a `pty` feature.)* |

#### 3.3.2 MCP Integration

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-MCP-01 | MCP client | `infra/mcp` implements `McpPort` over the MCP JSON-RPC transport (stdio + SSE planned). |
| FR-MCP-02 | Config-driven servers | `ag.toml` lists `[[mcp.servers]]` with `name`, `command`, `args`, `env`. |
| FR-MCP-03 | Tool discovery | On engine boot, the `ToolRegistry` calls each MCP server's `tools/list` and exposes them as agent tools. |
| FR-MCP-04 | Tool execution | Agent tool calls route to `McpPort::call(name, args)` → MCP `tools/call`. |
| FR-MCP-05 | Graceful degradation | An MCP server that fails to start is logged and skipped; the agent runs with remaining tools. |

#### 3.3.3 LSP Integration

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-LSP-01 | LSP client | `infra/lsp` implements `LspPort` using the `lsp-types` + `tower-lsp`/`rust-analyzer`-wire protocol. |
| FR-LSP-02 | Semantic tooling | Exposes `goto_definition`, `find_references`, `hover`, `rename_symbol` as agent tools. |
| FR-LSP-03 | Per-language server registry | `ag.toml` maps file extensions → LSP server command (`rust_analyzer`, `pyright`, etc.). |
| FR-LSP-04 | Document sync | LSP client opens files on agent `read`, keeps a text-document state, and pushes `didChange` as edits happen. |

### 3.4 Model & Provider Agnosticism

The LLM layer is a single `LlmPort` trait with concrete provider clients in `infra/llm/*`. Config selects the provider + model; the client is constructed in `wire()`.

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-MODEL-01 | OpenAI client | `OpenAiLlm` (replaces the v0.1.0 stub) hits `https://api.openai.com/v1/chat/completions` with Bearer token from `AG_OPENAI_API_KEY`. |
| FR-MODEL-02 | Anthropic client | `AnthropicLlm` via `https://api.anthropic.com/v1/messages` with `AG_ANTHROPIC_API_KEY`. |
| FR-MODEL-03 | OpenRouter client | `OpenRouterLlm` via `https://openrouter.ai/api/v1/chat/completions` with `AG_OPENROUTER_API_KEY`. |
| FR-MODEL-04 | Ollama client | `OllamaLlm` via `http://localhost:11434/api/chat` (local, streaming). |
| FR-MODEL-05 | vLLM client | `VllmLlm` via OpenAI-compatible endpoint (reuse `OpenAiLlm` with custom endpoint). |
| FR-MODEL-06 | Provider dispatch | `wire()` reads `config.provider` and constructs the matching `Box<dyn LlmPort>`. Unknown provider → typed `AppError`. |
| FR-MODEL-07 | Streaming | All clients stream via Server-Sent Events / newline-delimited JSON; engine forwards deltas to the UI. |
| FR-MODEL-08 | Multi-modal | Image inputs accepted (`--image <path>` → base64 `image_url` for OpenAI/OpenRouter/Anthropic; text-only for Ollama/vLLM with a warning). |

### 3.5 Agent Mode Switching

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-MODE-01 | Planning mode | `--mode planning` restricts the tool set to read-only tools (`read`, `list_dir`, `hover`, MCP reads); LLM is prompted to ask for confirmation; the engine refuses execute-side tools. |
| FR-MODE-02 | Build mode | `--mode build` enables full tool set (`write`, `str_replace`, `shell`, `rename_symbol`); LLM is prompted to act autonomously. |
| FR-MODE-03 | Mode-prompt templates | Each mode carries a system-prompt template in `domain::modes::templates.rs` (orchestration-only, no external files). |
| FR-MODE-04 | Mode as session metadata | The chosen mode is recorded per-step in the session/telemetry so planners can correlate behavior. |

### 3.6 Structured Output & Telemetry

The agent emits machine-readable output per requirement 5 ("Generate Use JSON output") and collects the required metrics.

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-OUTPUT-01 | `--json` streaming | In headless mode, each LLM delta, tool call decision, tool result, and terminal answer is emitted as a JSON object on stdout (one per line, JSONL). |
| FR-OUTPUT-02 | Report file | On completion, write `.ag/reports/<timestamp>-<session>.json` with the full run. |
| FR-OUTPUT-03 | Input token count | Count prompt tokens (approx via a lightweight tokenizer or provider-reported) per step. |
| FR-OUTPUT-04 | Output token count | Count generated completion tokens per step. |
| FR-OUTPUT-05 | Cache token count | Count provider-emitted cache tokens (Anthropic `cache_creation_output_tokens`, OpenAI `prompt_tokens_details`) when available; 0 otherwise. |
| FR-OUTPUT-06 | Execution step count | Increment a counter per LLM-turn + tool-call round. |
| FR-OUTPUT-07 | Execution time | Wall-clock from engine start to final answer (ms). |
| FR-OUTPUT-08 | Model name | Record provider + model identifier in every telemetry event. |
| FR-OUTPUT-09 | Skill folder access | Tool `ag:skill` reads a markdown file from `.ag/skills/` (or the configured skills dir) and injects it as context; skill path access is allowlisted by directory. |

### 3.7 Configuration & Allowed Commands

Configuration lives in `ag.toml` (see existing `examples/ag.example.toml` extended below) plus `AG_*` env overrides (precedence: env > file).

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-CONFIG-01 | Extend `ag.toml` schema | Add `provider`, `model`, `api_key_env` (key env-var name), `[[mcp.servers]]`, `[lsp.servers]`, `shell.allowed` (list of regex patterns), `skills_dir`, `mode`. |
| FR-CONFIG-02 | Provider selection | `config.provider` ∈ {openai, anthropic, openrouter, ollama, vllm, openai-compatible}. |
| FR-CONFIG-03 | Env-var secret resolution | API keys are read by the *name* in `api_key_env` (e.g. `AG_OPENAI_API_KEY`) at `wire()` time; never persisted. |
| FR-CONFIG-04 | Shell allowlist | `shell.allowed` is a list of regexes; a command runs iff **every token/segment** matches at least one allowed pattern. Default: `["echo .*", "ls .*", "cd .*", "cat .*"]`. Empty list → block all (fail-safe). |
| FR-CONFIG-05 | Command execution gating | `ShellPort::run` is wrapped by a `GuardedShell` decorator that applies the allowlist *before* dispatching to `StdShell` — satisfying "execute every command based on allowed on the configuration". |
| FR-CONFIG-06 | Skills dir | `skills_dir` defaults to `.ag/skills`; `ag:skill <name>` reads `<skills_dir>/<name>.md`. |

### 3.8 Engine Orchestration Loop (the heart)

The `App` orchestrator (upgraded from the v0.1.0 stub) runs:

```text
loop:
  1. render context (history + tool results) -> LLM
  2. LLM streams deltas; emit JSON if --json
  3. LLM emits a finish_reason:
       - "tool_use": pick the tool, dispatch, capture result, push to history, loop
       - "stop": final answer emitted, terminate
       - "length": terminate with truncation note
  4. on each turn: record tokens, time, model; append checkpoint
  max_turns / max_tokens cap terminates the loop
```

| ID | Requirement | Description |
|----|-------------|-------------|
| FR-LOOP-01 | Tool-use loop | Engine supports ≥ 10 tool-call rounds per task before `max_turns` cutoff. |
| FR-LOOP-02 | Turn cap | `config.max_turns` (default 20) hard-stops the loop and reports `truncated: true`. |
| FR-LOOP-03 | Token cap | `config.max_tokens` (default 16384) bounds LLM output; loop stops at the cap. |
| FR-LOOP-04 | Tool result truncation | Oversized tool results are trimmed to `config.max_tool_output_chars` (default 16000) with a truncation note. |

### 3.9 CLI Command Matrix

| Command | Interface | Purpose |
|---------|-----------|---------|
| `ag version` | headless | Build metadata (unchanged). |
| `ag run "<prompt>"` | headless | Single-run agent turn. |
| `ag` / `ag repl` | TUI | Interactive session. |
| `ag session create` | headless | New session ID. |
| `ag session continue <id>` | headless/TUI | Resume. |
| `ag session fork <id> --as <new>` | headless | Branch. |
| `ag session import <file>` | headless | Ingest external JSON. |
| `ag session export <id> --to <file>` | headless | Emit JSON. |
| `ag tools list` | headless | Enumerate available tools. |
| `ag skills list` | headless | Enumerate `.ag/skills/*.md`. |

---

## 4. Functional Requirements Summary (Traceability)

| Requirement area | FR IDs |
|------------------|--------|
| Multi-Interface Architecture | FR-IFACE-01 … FR-IFACE-06 |
| Session & State Management | FR-SESSION-01 … FR-SESSION-07 |
| Extensible Tool System (native) | FR-TOOL-FS-01/02/03, FR-TOOL-SHELL-01/02 |
| MCP Integration | FR-MCP-01 … FR-MCP-05 |
| LSP Integration | FR-LSP-01 … FR-LSP-04 |
| Model & Provider Agnosticism | FR-MODEL-01 … FR-MODEL-08 |
| Agent Mode Switching | FR-MODE-01 … FR-MODE-04 |
| Structured Output & Telemetry | FR-OUTPUT-01 … FR-OUTPUT-09 |
| Configuration & Allowlist | FR-CONFIG-01 … FR-CONFIG-06 |
| Engine Loop | FR-LOOP-01 … FR-LOOP-04 |
| CLI Matrix | — (implicit in §3.9) |

---

## 5. Non-Functional Requirements

### 5.1 Performance & Memory

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-PERF-01 | Cold start (`ag version`) | < 300 ms on 2023 laptop (carried from v0.1.0). |
| NFR-PERF-02 | Single-run task latency | `ag run "<small task>"` completes a 1-turn edit in < 2 s wall-clock excluding LLM network RTT. |
| NFR-PERF-03 | Hot-loop memory | A 20-step task holds < 8 MB process RSS above baseline (measured via `valgrind massif` / `ps`). |
| NFR-PERF-04 | No GC pauses | Pure Rust, no GC; latency bounded by I/O and provider RTT only. |
| NFR-PERF-05 | Release profile | `lto = thin`, `codegen-units = 1`, `panic = abort`, `strip = symbols` (unchanged). |

### 5.2 Reliability

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-REL-01 | Deterministic tests | `cargo test` green on a clean clone, no flakiness; network-touching tests gated behind `#[ignore]` or a `network` feature. |
| NFR-REL-02 | Fail-fast composition | `wire()` returns typed `AppError` on missing key/provider; never panics with a stack trace. |
| NFR-REL-03 | Crash recovery | Session auto-checkpoint means a killed process resumes from the last completed step. |
| NFR-REL-04 | No resource leaks | HTTP clients, LSP transports, MCP servers, and PTYs are dropped/closed on exit via RAII guards. |

### 5.4 Maintainability & Quality

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-MAINT-01 | Lint clean | `cargo clippy --workspace -- -D warnings` passes. |
| NFR-MAINT-02 | Format clean | `cargo fmt --check` passes. |
| NFR-MAINT-03 | Docs build | `cargo doc --no-deps --workspace` builds, 0 errors. |
| NFR-MAINT-04 | Test hermeticity | Domain/App tests are pure; infra tests use `tempfile` + local servers or are `#[ignore]`'d. |
| NFR-MAINT-05 | Architecture lint | `make check-deps` (acyclic graph: cli → app/infra/* → domain) still green after new crates. |

### 5.5 Portability & Security

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-PORT-01 | Tier-1 targets | Builds on Linux x86_64 and macOS aarch64. |
| NFR-PORT-02 | No unsafe | Zero `unsafe` in Domain/App; `unsafe` confined to infra crates that need PTY (`infra/shell`) and is `#![forbid]`-gated per crate. |
| NFR-SEC-01 | No secrets in repo | API keys never written to disk; `.ag/skills`, `.ag/sessions`, `ag.toml.local` gitignored. |
| NFR-SEC-02 | Shell sandboxing | `shell.allowed` regex allowlist enforced before any `std::process::Command::spawn`; default-deny. |
| NFR-SEC-03 | Supply chain | `deny.toml` + `cargo audit` configured for CI. |
| NFR-SEC-04 | TUI escapes | ratatui output is sanitized; no raw terminal escape injection from tool results. |

### 5.6 Observability (built by FR-OUTPUT-*)

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-OBS-01 | JSONL emit | `ag run --json` emits one JSON object per event; parseable by `jq`. |
| NFR-OBS-02 | Telemetry schema | Every `.ag/reports/*.json` matches a documented schema (model, input_tokens, output_tokens, cache_tokens, steps, execution_time_ms, session_id). |

---

## 6. Out of Scope

The following are deliberately excluded from v0.2.0; they land in later milestones. Resist scope creep.

1. **Full OpenCode plugin ecosystem (JS/WASM plugins)** — v0.2.0 exposes MCP + LSP + native Rust tools only.
2. **Multi-turn conversational memory across sessions** — sessions persist, but cross-session context is not stitched.
3. **Image generation / multimodal output** — image *input* (vision) is supported; image *generation* is not.
4. **Agent-as-a-service / daemon mode** — all runs are ephemeral processes.
5. **Bundled model runtime** — no in-process LLM; all providers are HTTP.
6. **Rich markdown rendering in TUI** — plain text + minimal syntax highlighting only.
7. **Git integration beyond `shell`** — the agent can run `git` via the allowlisted shell tool, but there is no first-class GitPort in this milestone.
8. **Voice / audio input** — not supported.
9. **Collaborative / multi-agent orchestration** — single-agent loop only.
10. **Windows PTY** — the `pty` feature is Unix-only in v0.2.0 (Windows TUI uses `conhost` shim).
11. **Auto-updater** — not shipped.
12. **CI workflow files** — out of scope (Makefile local runner only, per v0.1.0 decision).

---

## 7. Success Metrics

### 7.1 Primary (must-hit) — Core Capability Readiness

| Metric | Target | Measurement |
|--------|--------|-------------|
| M1.1 Build green | `cargo build --workspace` succeeds, 0 warnings | CI / local |
| M1.2 Tests green | `cargo test --workspace` passes (network tests `#[ignore]`'d) | CI / local |
| M1.3 Lint green | `cargo clippy --workspace -- -D warnings` + `cargo fmt --check` pass | CI / local |
| M1.4 Architecture lint | `make check-deps` acyclic graph still green after new crates | CI / local |
| M1.5 End-to-end edit | `ag run "rename function foo to bar in crates/domain/src/model.rs"` performs the rename correctly | Manual + scripted acceptance |
| M1.6 Headless JSON | `ag run --json "<task>"` emits valid JSONL (parseable by `jq -e .`) | Automated check |
| M1.7 Telemetry present | `.ag/reports/*.json` contains `model`, `input_tokens`, `output_tokens`, `steps`, `execution_time_ms` | Schema check |
| M1.8 Session lifecycle | `ag session create` → `export` → `import` of an external JSON session succeeds | Manual + scripted |
| M1.9 MCP tool discovery | A fixture MCP server (`mcp-everything`) exposes ≥ 1 tool to `ag tools list` | Integration test (ignore'able) |
| M1.10 Shell allowlist | A blocked command (`rm -rf /`) is refused; an allowed command (`echo hi`) runs | Test in `crates/infra/shell` |
| M1.11 TUI launches | `ag repl` renders a ratatui screen and exits cleanly on `q` | Manual smoke |

### 7.2 Secondary — Performance & Security

| Metric | Target | Measurement |
|--------|--------|-------------|
| M2.1 Cold start | `ag version` < 300 ms (release) | `time` on 2023 laptop |
| M2.2 Task latency | single-turn file edit (excl. network) < 2 s | `hyperfine` on `ag run` with a local Ollama provider |
| M2.3 Memory ceiling | 20-step task < 8 MB RSS above baseline | `valgrind massif` or `ps` sample |
| M2.4 Binary size | release `ag` < 12 MB | `du -h target/release/ag` |
| M2.5 Shell deny-all | empty `shell.allowed` → no command runs | Test |
| M2.6 No secrets leak | grep repo for `*.api_key` patterns → 0 hits | `make secrets-scan` |

### 7.3 Leading Indicators — Forward-looking (not gates)

| Metric | Threshold (early signal) | Note |
|--------|--------------------------|------|
| L1 Provider coverage | ≥ 3 providers (OpenAI, Anthropic, OpenRouter) compile & dispatch | CI matrix |
| L2 MCP servers | ≥ 1 real MCP server works end-to-end | `mcp-everything` |
| L3 LSP attach | rust-analyzer attaches to a fixture repo and `find_references` resolves | Integration test (ignore'able) |
| L4 Compile time | `cargo build --release` cold < 240 s on CI runner | Signals healthy crate size |

> A milestone is considered **successful** when all of section 7.1 is green, section 7.2 is within threshold, and no `MUST-FIX` issues remain (CRITICAL/High). Section 7.3 indicators are signals, not gates.

---

## 8. Assumptions & Constraints

- The user base for v0.2.0 is **early adopters / contributors**; production hardening (rate limiting, retries, circuit-breakers) is staged in v0.3.0.
- The v0.1.0 Clean Architecture boundaries (Domain pure, App→Domain, Infra→Domain, CLI→all) are **frozen** and must hold.
- All LLM providers are reached over **HTTP**; no in-process inference in v0.2.0.
- MCP stdio transport only in v0.2.0 (SSE transport deferred to v0.3.0).
- The `skills_dir` feature is read-only; skills are markdown context snippets, not executable code.
- Rust edition **2021**, toolchain **1.85** (carried from v0.1.0; no 1.85-specific features required).
- `tokio` runtime stays single-threaded (`flavor = "current_thread"`) for minimal idle memory; concurrency comes from async streams, not thread pools, in v0.2.0.

---

## 9. Open Questions (to resolve during sprint)

1. **PTY crate choice:** `repro-get` + `rustix` vs. `tokio-util` + `winapi` shim — pick one that compiles on Unix first, Windows in v0.2.1.
2. **LSP client library:** `lsp-types` + hand-rolled JSON-RPC vs. `tower-lsp` client mode — prefer the lighter `lsp-types` + `tokio` channel approach to minimize deps.
3. **MCP transport:** use `mcp-rust` ecosystem crate or implement JSON-RPC stdio directly? Decision hinges on whether the upstream crate is on a 1.85-compatible release.
4. **Token counting:** ship a tiny whitespace/word heuristic in `domain` or rely on provider-reported usage fields? Tentative: use provider-reported `usage` for accuracy, heuristic as fallback.
5. **TUI framework:** `ratatui` is the clear choice (matches the codebase's lean ethos); confirm no extra features beyond `crossterm` backend.
6. **HTTP client:** `reqwest` (with `rustls-tls`) is the de-facto standard; weigh against `isahc` for lower binary size — decision recorded here.
7. **Session ID format:** UUIDv7 (time-ordered, better for sorting) vs. nanoid (short) — recommend UUIDv7 via the `uuid` crate's `v7` feature.

---

## 10. Milestone Dependencies & Backlog Seed

This PRD feeds directly into the task breakdown:

- **task-12:** Upgrade `infra/llm` to multi-provider clients (OpenAI, Anthropic, OpenRouter, Ollama).
- **task-13:** Add `crates/infra/mcp` — MCP client + `McpPort`.
- **task-14:** Add `crates/infra/lsp` — LSP client + `LspPort`.
- **task-15:** Add `crates/tools` — native `read`/`write`/`str_replace`/`shell` `Tool` impls + `ToolRegistry`.
- **task-16:** Wire `App::execute` orchestration loop (tool-use → dispatch → checkpoint → stream).
- **task-17:** Add TUI interface (`crates/cli/tui/`) + headless `run` subcommand.
- **task-18:** Session store + import/export JSON schema.
- **task-19:** Telemetry schema + JSONL output + report file.
- **task-20:** Config schema extension (providers, MCP, LSP, allowlist, skills, modes).

---

*End of document.*
