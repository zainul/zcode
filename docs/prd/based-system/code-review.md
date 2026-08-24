# Code Review — based-system (v0.2.0)

**Review date:** 2026-08-24
**Branch:** `develop-release-based-system`
**Reviewer:** Tech Lead (code review pass)
**PRD reviewed:** `docs/prd/based-system/prd.md`
**Tech plan reviewed:** `docs/prd/based-system/technical-plan.md`
**Tasks reviewed:** `docs/prd/based-system/tasks/task-[12..20]*.md`

---

## 1. Executive Summary

The `based-system` milestone delivered the **domain-layer foundation and several
fully-implemented, well-tested infrastructure crates**, but the **five central
integration crates remain stubs**. The workspace compiles cleanly, all 53 tests
pass, clippy is warning-free, and `cargo doc` builds. However, the end-to-end
agent loop, the tool registry, the MCP/LSP clients, the TUI, and the headless
`run`/`session`/`tools`/`skills` CLI subcommands are **not yet implemented**.

**Bottom line:** substantial upward progress — the "skeleton" is now
fully-fleshed at the domain/port level — but the milestone is **not shippable**
as described in PRD §7.1 (no end-to-end edit, no JSONL, no dual interfaces, no
shell allowlist enforcement, no MCP L2/LSP L3). The remaining tasks
(task-13, task-14, task-15, task-16, task-17) carry the user-facing value and
are the critical path to M1.5–M1.11.

---

## 2. What Was Built (file-by-file inventory)

| Crate / File | Status | Lines | Tests | Notes |
|---|---|---|---|---|
| `crates/domain/src/ports.rs` | **IMPLEMENTED** | 438 | 3 | Evolved `LlmPort`, new `Tool`, `ToolRegistryPort`, `McpPort`, `LspPort`, `SessionStorePort`, `TelemetryPort`, `Emitter`, `UiEvent` |
| `crates/domain/src/model.rs` | **IMPLEMENTED** | 154 | — | `AgentMode`, `ImageRef`, `CancelFlag`, `LspLocation`/`LspRange`/`LspWorkspaceEdit`, `ShellCommand`, `Session` |
| `crates/domain/src/modes.rs` | **IMPLEMENTED** | 37 | — | `system_prompt()`, `execute_only_tool_names()`, `is_execute_only()` |
| `crates/domain/src/tokens.rs` | **IMPLEMENTED** | 9 | — | `estimate_tokens()` heuristic fallback |
| `crates/domain/src/error.rs` | **IMPLEMENTED** | 22 | — | `DomainError` enum |
| `crates/domain/src/lib.rs` | **IMPLEMENTED** | 36 | — | Re-exports all types |
| `crates/infra/llm/src/lib.rs` | **IMPLEMENTED** | 1023 | 8 | 4 providers (OpenAi/OpenRouter/Vllm/Anthropic/Ollama), SSE+NDJSON parsers |
| `crates/infra/llm/Cargo.toml` | **IMPLEMENTED** | 16 | — | Deps: domain, serde, serde_json, reqwest, thiserror |
| `crates/infra/config/src/lib.rs` | **IMPLEMENTED** | 485 | 9 | Extended `Config`, `Provider`, `McpServerConfig`, `LspServerConfig`, `Loader` |
| `crates/infra/config/Cargo.toml` | **IMPLEMENTED** | 15 | — | Deps: domain, serde, toml, thiserror |
| `crates/infra/session/src/lib.rs` | **IMPLEMENTED** | 650 | 13 | `UuidSessionStore`, atomic checkpoints, fork/import/export |
| `crates/infra/session/Cargo.toml` | **IMPLEMENTED** | 15 | — | Deps: domain, uuid, serde, serde_json |
| `crates/infra/telemetry/src/lib.rs` | **IMPLEMENTED** | 487 | 7 | `JsonTelemetry`, JSONL emit, report file, `ExtraField` bridge |
| `crates/infra/telemetry/Cargo.toml` | **IMPLEMENTED** | 14 | — | Deps: domain, serde, serde_json |
| `crates/infra/filesystem/src/lib.rs` | **UNCHANGED** | 121 | 5 | v0.1 `StdFs`, works |
| `crates/infra/shell/src/lib.rs` | **UNCHANGED** | 134 | 3 | v0.1 `StdShell`, `spawn()` deferred, `run()` implemented |
| `crates/infra/mcp/src/lib.rs` | **STUB** | 2 | 0 | Only `#![forbid(unsafe_code)]` |
| `crates/infra/lsp/src/lib.rs` | **STUB** | 2 | 0 | Only `#![forbid(unsafe_code)]` |
| `crates/tools/src/lib.rs` | **STUB** | 2 | 0 | Only `#![cfg_attr(not(test), forbid(unsafe_code))]` |
| `crates/tools/Cargo.toml` | **DEFINED** | 34 | — | Declares deps (infra-mcp/lsp optional), but no code |
| `crates/app/src/lib.rs` | **STUB** | 237 | 2 | Still v0.1 architecture: `Arc<dyn…+Sync>`, `TaskRunner`/`EditPlanner` stubs |
| `crates/app/Cargo.toml` | **UNCHANGED** | 10 | — | Still only domain + thiserror |
| `crates/cli/src/cli/mod.rs` | **STUB** | 111 | 3 | Only `Version` command; `wire()` is v0.1 Noop stub |
| `crates/cli/Cargo.toml` | **UPDATED** | 35 | — | Declares ratatui/crossterm/signal-hook/etc. deps, but none are used in code |
| `crates/cli/src/main.rs` | **UNCHANGED** | 12 | — | Still `#![forbid(unsafe_code)]`, `#[tokio::main]` |
| `crates/cli/build.rs` | **UNCHANGED** | 72 | — | Git SHA + profile embedding |
| `Cargo.toml` (workspace) | **UPDATED** | — | — | Added 5 new crates to members + workspace deps |
| `Cargo.lock` | **UPDATED** | — | — | +1368 lines (new transitive deps) |
| `crates/infra/config/examples/ag.example.toml` | **IMPLEMENTED** | 64 | — | Documents all new keys |
| `crates/infra/shell/Cargo.toml` | **UPDATED** | 24 | — | Added `pty` feature, `[lib] name` override |
| `Makefile` | **UNCHANGED** | 40 | — | `check-deps` only checks domain purity |
| `deny.toml` | **UNCHANGED** | 12 | — | Present but `cargo-audit` not installed |

---

## 3. PRD Coverage Analysis

### Implemented Requirements (PRD §3)

| Requirement | Status | Notes |
|---|---|---|
| **FR-MODEL-01..08** Provider agnosticism | ✅ FULL | `infra/llm` covers OpenAI, Anthropic, OpenRouter, Ollama, vLLM/OpenAI-compatible. Streaming, token reporting, multi-modal warnings. 8 unit tests. |
| **FR-CONFIG-01..06** Configuration | ✅ FULL | `infra/config` has all fields, `Provider` enum (6 variants), `Loader` with env-over-file precedence, `resolve_api_key` by-name, allowlist defaults, skills_dir. 9 tests. |
| **FR-SESSION-01..07** Sessions | ✅ FULL | `infra/session` `UuidSessionStore` implements all 6 methods, atomic checkpoint, UUIDv7 validation, path-traversal protection, import/export JSON. 13 tests. |
| **FR-OUTPUT-01..08** Telemetry | ✅ FULL | `infra/telemetry` `JsonTelemetry` emits JSONL, writes report file with documented schema, `ExtraField` bridge preserves domain purity. 7 tests. |
| **FR-OUTPUT-09** Skill folder access | ⚠️ PARTIAL | `ag:skill` tool spec is defined in `domain::modes` and task-15, but the actual `SkillTool` in `crates/tools` is a stub. `skills_dir()` config helper exists but no tool implements it. |
| **FR-DI-01/02** Architecture boundaries | ✅ FULL | `domain` has zero third-party deps (verified: `cargo tree -p domain` = 1 line); `app` has only `domain` + `thiserror`. |

### Not Yet Implemented (PRD §3)

| Requirement | Status | Blocking crate |
|---|---|---|
| **FR-IFACE-01/02/03/04/05/06** Multi-Interface Architecture | ❌ MISSING | CLI only has `version`; no `run`, `repl`, `session`, `tools`, `skills` subcommands |
| **FR-TOOL-FS-01/02/03** Native file tools | ❌ MISSING | `crates/tools` is a stub |
| **FR-TOOL-SHELL-01/02** Shell tool + allowlist | ❌ MISSING | `crates/tools` stub; `GuardedShell` not implemented; no allowlist enforcement |
| **FR-MCP-01..05** MCP integration | ❌ MISSING | `crates/infra/mcp` is a stub |
| **FR-LSP-01..04** LSP integration | ❌ MISSING | `crates/infra/lsp` is a stub |
| **FR-MODE-01..04** Mode switching | ⚠️ PARTIAL | `domain::modes::system_prompt()` + `is_execute_only()` exist, but the engine loop that enforces mode gating is not implemented |
| **FR-LOOP-01..04** Engine loop | ❌ MISSING | `crates/app` still uses v0.1 `TaskRunner`/`EditPlanner` stubs; no `AgentLoop::execute` |
| **FR-SESSION-06** Auto-checkpoint per tool round | ⚠️ PARTIAL | `SessionStorePort::checkpoint()` is implemented, but the engine loop that calls it does not exist |
| **FR-CONFIG-05** `GuardedShell` decorator | ❌ MISSING | Not implemented in `crates/tools` |

### PRD Success Metrics (§7.1)

| Metric | Status | Assessment |
|---|---|---|
| M1.1 Build green, 0 warnings | ✅ | `cargo build --workspace` succeeds |
| M1.2 Tests green | ✅ | 53 tests pass |
| M1.3 Clippy + fmt clean | ⚠️ | Clippy clean; `cargo fmt --check` has 2 minor diffs (see §6) |
| M1.4 Architecture lint (acyclic graph) | ✅ | `cargo tree -p domain` pure; `cargo tree -p app` = domain + thiserror |
| M1.5 End-to-end edit | ❌ | Cannot run — no `ag run` command |
| M1.6 Headless JSONL | ❌ | No `ag run --json` command |
| M1.7 Telemetry report schema | ✅ (unit) | `infra/telemetry` tests verify schema; but not wired to CLI |
| M1.8 Session lifecycle | ✅ (unit) | `infra/session` tests cover create/continue/fork/export/import; no CLI subcommand |
| M1.9 MCP tool discovery | ❌ | MCP crate is a stub |
| M1.10 Shell allowlist | ❌ | `GuardedShell` not implemented |
| M1.11 TUI launches | ❌ | No `ag repl` command |

### PRD Secondary Metrics (§7.2)

M2.1 (cold start < 300ms), M2.4 (binary < 12MB) — **cannot evaluate** because the
CLI doesn't wire up the TUI/ratatui yet. The binary currently builds at v0.1.0 size.

---

## 4. Code Quality Observations

### 4.1 Strengths

1. **Domain layer is exemplary.** `ports.rs` (438 lines) defines a complete,
   well-documented set of port traits with owned types (no lifetimes), `BoxError`
   type alias for `Send + Sync` cross-thread safety, and `Emitter`/`UiEvent`
   abstractions that cleanly separate engine from interface. This is clean
   architecture done right.

2. **infra/llm is production-grade.** The `OpenAiShapeLlm` abstraction (shared by
   OpenAI/OpenRouter/vLLM) is a textbook DRY refactor. The SSE/NDJSON parsers
   (`parse_openai_events`, `parse_anthropic_events`, `parse_ollama_events`) are
   correct, well-tested with canned payloads, and handle edge cases (tool call
   delta accumulation, `[DONE]` skipping, usage extraction). The Ollama vision
   warning (FR-MODEL-08) is implemented per spec.

3. **infra/session is robust.** UUIDv7 generation and validation (rejecting v4,
   `..`, `/`), atomic write-via-temp-rename, `SessionFile` serialization adapter
   keeping domain serde-free, and 10 thorough tests including crash-simulation.

4. **infra/telemetry is correct by design.** The `ExtraField` enum correctly
   bridges domain→JSON without leaking `serde_json` into domain. Token
   accumulation via `max()` is the right strategy for provider-reported
   accuracy. Report schema matches the documented M1.7 keys exactly.

5. **infra/config is complete.** `ConfigFile`/`McpSection`/`LspSection`
   deserialization with `Option` fields preserves v0.1 defaults when keys are
   absent. Provider dispatch enums cover all 6 values. Env-over-file
   precedence is correct.

6. **Security boundaries are respected.** `domain` has zero third-party deps
   (verified). `app` has only `domain` + `thiserror`. `regex` is confined to
   `crates/tools` only (not in config). `#![forbid(unsafe_code)]` holds in
   domain, telemetry, session, mcp, lsp, and CLI.

7. **Build metadata is solid.** `build.rs` replaces the `vergen-gix` crate
   (which needs Rust ≥ 1.88) with pure-std `git rev-parse` + `.git/HEAD` fallback.

### 4.2 Concerns

1. **`crates/cli/Cargo.toml` declares 9 new dependencies** (ratatui, crossterm,
   signal-hook, uuid, serde_json, infra-mcp, infra-lsp, infra-session,
   infra-telemetry, tools) that are **never used** in `cli/src/cli/mod.rs`.
   These are "prepared" deps for task-17, but having them resolved in
   `Cargo.lock` inflates the dependency graph prematurely. This is acceptable
   as forward preparation but should be noted.

2. **`crates/app/src/lib.rs` is still entirely v0.1.** It uses `Arc<dyn … +
   Sync>` instead of the planned `Box<dyn … + Send>`, keeps the old
   `FileSystemPort`/`ShellPort`/`PluginRegistryPort` fields (which should have
   moved into `ToolRegistry` per task-15), and the `TaskRunner`/`EditPlanner`
   traits return `"not implemented in v0.1.0"` errors. The evolved `LlmPort`
   and new `ToolRegistryPort`/`SessionStorePort`/`TelemetryPort` traits
   defined in domain are **not consumed** by `App`.

3. **`crates/infra/shell/Cargo.toml` has a `[lib]` section** with
   `name = "infra_shell"` that is unnecessary — Cargo auto-derives the lib name
   from the package name. The `pty` feature is empty (no actual code gated
   behind it). This is harmless but inconsistent with the other infra crates.

4. **`Cargo.toml` workspace version is still `0.1.0`** despite the PRD targeting
   v0.2.0. The agent string hardcodes `"ag/0.2.0"` (`infra/llm/src/lib.rs:35,
   `user_agent("ag/0.2.0")`), but the package version and CLI version print are
   both `0.1.0`.

5. **`make check-deps` only checks domain purity**, not the
   `app`-no-third-party-deps invariant (NFR-MAINT/§5 of tech plan). The Makefile
   should also assert `cargo tree -p app` has no unexpected edges.

6. **`benches/benches/smoke.rs`** references `DomainError` (from `crate::model`)
   and the old `FileEdit`/`Task`/`TaskStatus` types. The benchmarks still
   compile against the v0.1 domain model surface, which is now superseded by
   `LlmMessage`/`LlmToolCall`/`LlmToolResult`. The benchmarks don't exercise the
   new v0.2 types — they should be updated or at minimum noted as v0.1 carryover.

---

## 5. Test Coverage

### 5.1 Current State

**53 tests pass across 11 crates** (0 failures, 0 ignored):

| Crate | Tests | Status |
|---|---|---|
| `cli` | 3 | ✅ `version_command_parses`, `git_sha_const_exists`, `wire_constructs_app` |
| `app` | 2 | ✅ `app_returns_port_error_for_run`, `app_returns_port_error_for_plan` (v0.1 stubs) |
| `domain` | 3 | ✅ `message_helpers_build_expected_roles`, `tool_result_helpers`, `completion_chunk_still_constructible` |
| `infra/config` | 9 | ✅ Coverage of env override, defaults, allowlist, provider, MCP parsing, API key |
| `infra/filesystem` | 5 | ✅ Round-trip, exists, list, read-missing, watch-stub |
| `infra/llm` | 8 | ✅ OpenAI tool-call SSE, stop/finish, Anthropic usage/tool-call, OpenRouter/vLLM endpoints, Ollama NDJSON + vision warning |
| `infra/mcp` | 0 | ❌ Stub — no tests |
| `infra/lsp` | 0 | ❌ Stub — no tests |
| `infra/session` | 13 | ✅ UUIDv7, checkpoint roundtrip, atomic-on-crash, fork, import/export, traversal, serialization |
| `infra/shell` | 3 | ✅ run-echo, spawn-pty-error, run-missing-command |
| `infra/telemetry` | 7 | ✅ JSONL line validity, report schema, atomic report, ExtraField bridge, totals accumulation, dir creation |
| `tools` | 0 | ❌ Stub — no tests |
| `benches` | — | Benchmark harness (criterion) |

### 5.2 Missing Test Coverage

The following test scenarios from the technical plan (§8 T4–T23) are **not
implemented** because the crates they target are stubs:

- **T10** (MCP tools/listed) — `infra/mcp` has no implementation or tests
- **T11** (LSP goto def) — `infra/lsp` has no implementation or tests
- **T13** `str_replace` roundtrip, `write` atomic, skill path traversal — `crates/tools` is a stub
- **T16–T17** engine loop tests (fake LLM, planning-mode refuse, max_turns truncate, max_tool_output_chars truncate, interrupt, history append) — `app` is still v0.1
- **T20** `ag run "rename foo to bar in crates/domain/src/model.rs"` — no `run` command
- **T21–T23** binary size, cold start timing — CLI not wired for TUI

### 5.3 Test Quality of Existing Code

The implemented crates have **excellent** hermetic test suites:
- No network/PTY/LSP/MCP-live tests are left un-`#[ignore]`d
- `tempfile` is used for filesystem/session/telemetry isolation
- Canned SSE/NDJSON payloads exercise the parsers deterministically
- The `extra_fields_serialize` test in telemetry is thorough (covers all 6 `ExtraField` variants)
- The session store tests simulate crash recovery with a fake `.tmp` file

---

## 6. Issues Found

### MUST-FIX (critical, blocks v0.2.0 release)

1. **`crates/app/src/lib.rs` is unchanged from v0.1.0.** The entire engine loop
   (task-16) is missing. `App` still uses `Arc<dyn … + Sync>`, keeps
   `FileSystemPort`/`ShellPort`/`PluginRegistryPort` fields, and implements
   `TaskRunner`/`EditPlanner` as no-op stubs returning `"not implemented in
   v0.1.0"`. This is the **single largest gap** — without `AgentLoop::execute`,
   `ExecutionRequest`/`ExecutionResult`, and mode gating, none of the
   end-to-end metrics (M1.5, M1.6) can pass.

2. **`crates/tools/src/lib.rs` is an empty stub.** No `ToolRegistry`, no
   `GuardedShell`, no native tools (`read`/`write`/`str_replace_editor`/`shell`/
   `ag:skill`). Without this, M1.9, M1.10, and the entire tool-use loop are
   impossible.

3. **`crates/infra/mcp/src/lib.rs` and `crates/infra/lsp/src/lib.rs` are empty
   stubs.** No `McpClient` or `LspClient` implementation. MCP/LSP tools cannot
   be discovered or called. FR-MCP-01..05 and FR-LSP-01..04 are entirely
   unmet.

4. **`crates/cli/src/cli/mod.rs` only supports `ag version`.** The `Commands`
   enum has only `Version`. There is no `Run`, `Repl`, `Session`, `Tools`, or
   `Skills` subcommand. `wire()` constructs the v0.1 `App` with Noop ports and
   a hardcoded stub LLM endpoint (`http://localhost:9999`). No provider
   dispatch, no ToolRegistry, no session/telemetry wiring.

### SHOULD-FIX (high, before merge)

5. **Workspace version is `0.1.0`** but the PRD targets v0.2.0. The
   `user_agent` string in `infra/llm` hardcodes `"ag/0.2.0"` while
   `CLI::VERSION` (from `CARGO_PKG_VERSION`) still says `0.1.0`. Fix the
   workspace version to `"0.2.0"` for consistency.

6. **`cargo fmt --check` fails** with two minor formatting diffs in
   `crates/infra/telemetry/src/lib.rs` (lines 328 and 462). Run `cargo fmt`
   to resolve (NFR-MAINT-02).

7. **`make check-deps` only checks domain purity.** It should also assert
   `cargo tree -p app` contains no third-party deps beyond `thiserror`
   (NFR-MAINT-05 / tech plan §7.14).

8. **`crates/infra/shell/Cargo.toml` has an unnecessary `[lib]` section**
   (`name = "infra_shell"`) inconsistent with sibling crates. The `pty`
   feature is empty.

### COULD-FIX (medium, nice-to-have)

9. **`benches/benches/smoke.rs`** benchmarks only v0.1 domain types
   (`DomainError`, `FileEdit`, `Task`). Should be updated to benchmark the new
   v0.2 types (e.g., `LlmMessage` construction, `ToolSpec` construction) or
   removed if not planning to benchmark them.

10. **`.gitignore` does not list `.ag/`** (the sessions/skills/reports
    directories). While `ag.toml.local` is ignored, the `.ag/sessions/`,
    `.ag/skills/`, and `.ag/reports/` directories created at runtime are not
    explicitly gitignored. Add `.ag/` to `.gitignore`.

11. **`deny.toml` exists but `cargo-audit` is not installed** in the
    environment. The `ci` Makefile target does not run `cargo audit`
    (NFR-SEC-03). Consider adding `cargo audit` to the CI pipeline.

12. **`infra/llm` blocks on HTTP inside `stream()`** — the `LlmPort::stream`
   trait returns `Box<dyn Iterator>`, and each provider adapter performs a
    blocking `reqwest` call then parses the full body. This is per DQ4
    (sync ports), but means the TUI must run the engine on a dedicated
    `std::thread` (as planned). This is architecturally sound but worth
    noting for the implementer of task-17.

13. **`infra/config` `Loader::load()` does not support `AG_SHELL_ALLOWED`
    env override** (the example toml mentions it at line 44, but the code only
    checks `AG_PROVIDER`, `AG_MODEL`, `AG_API_KEY_ENV`, `AG_BASE_URL`,
    `AG_WORKING_DIR`, `AG_TIMEOUT_MS`, `AG_MAX_TURNS`, `AG_MODE`). The
    `AG_SHELL_ALLOWED` override is documented but unimplemented.

---

## 7. Architecture / Dependency Flow Verification

### Verified (all green)

- `cargo tree -p domain` → **`domain v0.1.0`** only (1 line, pure stdlib) ✅
- `cargo tree -p app` → `domain` + `thiserror` only ✅
- `cargo tree -p infra-llm` → `domain, serde, serde_json, reqwest, thiserror` ✅ (matches task-12 spec)
- `cargo tree -p infra-config` → `domain, serde, toml, thiserror` (no `regex`/`reqwest`) ✅
- `cargo tree -p infra-session` → `domain, uuid, serde, serde_json` ✅
- `cargo tree -p infra-telemetry` → `domain, serde, serde_json` ✅
- `cargo tree -p infra-mcp` → `domain, serde, serde_json` (stub compiles but does nothing) ✅
- `cargo tree -p tools` → `domain, infra-filesystem, infra-shell, infra-config, regex, serde, serde_json, thiserror` ✅ (matches task-15 spec; `regex` confined here)
- `cargo tree -p ag` (cli) → reaches all layers ✅
- `make check-deps` → "domain pure OK" ✅

### Acyclic graph

The workspace dependency graph is acyclic and follows the frozen
convention `cli → app/infra/* → domain`:

```
cli ─► app ─► domain (pure)
cli ─► infra/{llm,mcp,lsp,session,telemetry,filesystem,shell,config} ─► domain
cli ─► crates/tools ─► infra/{filesystem,shell,config} ─► domain
cli ─► benches ─► domain
```

The `tools` crate's optional `infra-mcp`/`infra-lsp` features (DQ10) are
correctly declared as optional deps.

---

## 8. Recommendations

### Immediate (before this branch can ship)

1. **Implement `crates/tools/src/lib.rs`** (task-15). This is the linchpin:
   `GuardedShell`, `FsReadTool`, `FsWriteTool`, `StrReplaceTool`, `ListDirTool`,
   `ShellTool`, `SkillTool`, and the `ToolRegistry` that merges native+MCP+LSP.
   The domain traits (`Tool`, `ToolRegistryPort`) are already defined and
   correct — this is pure implementation work.

2. **Implement `crates/app/src/lib.rs`** (task-16). Evolve `App` to own
   `Box<dyn LlmPort + Send>`, `Box<dyn ToolRegistryPort + Send>`,
   `Box<dyn SessionStorePort + Send>`, `Box<dyn TelemetryPort + Send>`,
   `Box<dyn LoggerPort + Send>`. Implement `AgentLoop::execute` with the
   PRD §3.8 loop, mode gating, turn/token caps, truncation, and cancellation.
   Remove the old `Arc<dyn … + Sync>` pattern and `TaskRunner`/`EditPlanner`.

3. **Implement `crates/infra/mcp/src/lib.rs`** (task-13). `McpClient` with
   stdio JSON-RPC (`initialize`/`tools/list`/`tools/call`), graceful
   degradation on server failure, `McpPort` impl.

4. **Implement `crates/infra/lsp/src/lib.rs`** (task-14). `LspClient` with
   stdio JSON-RPC, `goto_definition`/`find_references`/`hover`/`rename_symbol`/
   `open_document`, `LspPort` impl.

5. **Evolve `crates/cli/src/cli/mod.rs`** (task-17). Add `Run`, `Repl`,
   `Session`, `Tools`, `Skills` subcommands. Rewrite `wire(&Config)` to
   dispatch providers, build `ToolRegistry`, wire session+telemetry, and call
   `App::execute`. Implement the ratatui TUI.

6. **Bump workspace version to `0.2.0`** and update `ag version` to reflect
   the new release.

7. **Run `cargo fmt`** to fix the two formatting diffs.

### Quality Bar (before merge)

8. All hermetic test scenarios from the tech plan §8 (T4–T23) should be
   implemented as unit tests using the `FakeLlm`/`FakeToolRegistry`/
   `FakeSessionStore`/`FakeTelemetry` pattern described in task-16 §7. The
   existing infra crates demonstrate an excellent hermetic-test standard —
   the app/tools crates should match it.

9. **Extend `make check-deps`** to also verify `cargo tree -p app` has no
   third-party edges beyond `thiserror`:

   ```makefile
   A_LINES=$$(cargo tree -p app 2>&1 | tail -n +2 | grep -c '├──\|└──'); \
   ```

10. **Add `.ag/` to `.gitignore`** (sessions, skills, reports are runtime
    artifacts).

11. **Add `cargo audit` to CI** (NFR-SEC-03) once `cargo-audit` is available,
    or document its manual run.

12. **Implement `AG_SHELL_ALLOWED` env override** in `infra/config` (documented
    in the example toml but missing from `Loader::load()`).

### Ordering Recommendation

The task roadmap in the tech plan §12 is correct. Implement in dependency
order:

```
task-15 (tools) → task-13 (mcp) ┴
task-14 (lsp)  ┴                 ├─► task-16 (app engine loop)
task-12 (llm) [done] ────────────┤     ↓
task-18 (session) [done] ────────┤   task-17 (CLI interfaces)
task-19 (telemetry) [done] ──────┤
task-20 (config) [done] ─────────┘
```

The domain layer is the solid foundation; the remaining 5 tasks are the
integration layers that turn stubs into a working agent.

---

## 9. Overall Assessment

| Criterion | Verdict |
|---|---|
| Does it build? | ✅ `cargo build --workspace` — 0 warnings |
| Do tests pass? | ✅ 53/53 pass, 0 failures |
| Is clippy clean? | ✅ `cargo clippy --workspace -- -D warnings` |
| Do docs build? | ✅ `cargo doc --no-deps --workspace` |
| Is domain pure? | ✅ `cargo tree -p domain` = 1 line |
| Is the agent functional (end-to-end)? | ❌ No `ag run`, no engine loop, no tools, no MCP/LSP, no TUI |
| Is v0.2.0 shippable per PRD §7.1? | ❌ 5 of 11 primary metrics (M1.5–M1.11) are unmet |

**Verdict: Approve-in-principle with MUST-FIX items.** The foundation is
excellent — the domain layer, LLM adapters, config, session store, and
telemetry crate are all production-quality with strong test coverage. However,
the milestone is **not complete**: the five integration crates (`app`, `tools`,
`infra/mcp`, `infra/lsp`, `cli`) are still v0.1 stubs. This branch should not
merge to main until tasks 13–17 are implemented and M1.5–M1.11 pass.

**Recommended next action:** Complete tasks 13–17 in dependency order (above).
The existing code provides a correct, tested contract surface — the remaining
work is integration and implementation against those contracts.

---

*End of review.*
