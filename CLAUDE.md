# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

zcode — a terminal coding agent in Rust with OpenCode-equivalent core
capabilities (agent loop, file edits, shell exec, MCP/LSP extensibility, multi
provider LLM) and a deliberately small memory and dependency footprint. Cargo
workspace, clean architecture (ports & adapters).

## Commands

All gates are wrapped by the `Makefile`; prefer these over raw cargo:

```sh
make ci          # fmt-check + lint + test + build + check-deps + secrets-scan
make test        # cargo test --workspace
make lint        # cargo clippy --workspace -- -D warnings
make check-deps  # asserts `cargo tree -p domain` is exactly 1 line (domain purity)
make check-arch  # full layer-topology check (docs/architecture/dependency-check.sh)
make bench       # criterion benches (crate `zcode-benches`)
```

Targeted runs — **package names differ from directory names**:

```sh
cargo test -p domain
cargo test -p zcode                           # crates/cli  (package and binary are `zcode`)
cargo test -p infra-llm parse_openai       # filter by test-name substring
cargo run -q -p zcode -- version
```

Directory → package: `crates/cli` → `zcode`, `crates/infra/<x>` → `infra-<x>`,
`crates/tools` → `tools`, `benches` → `zcode-benches`.

The toolchain is pinned to **1.85.0**; the first cargo invocation on a fresh
machine downloads it (slow, silent for a while).

## Architecture

```
cli ──► app ──► domain            domain is stdlib-only
cli ──► infra/* ──► domain        cli is the composition root
cli ──► tools ──► infra/{filesystem,shell,config,mcp,lsp}
```

Hard rules, enforced by `make check-deps` / `make check-arch` and cited in doc
comments as `FR-DI-0x`:

- **`crates/domain` has zero third-party dependencies.** Its `[dependencies]`
  is intentionally empty. Never add a crate here — not `serde`, not
  `thiserror`, not a tokenizer.
- `crates/app` depends on `domain` + `thiserror` only. No tokio, no serde, no HTTP.
- `crates/infra/*` and `crates/tools` depend on `domain` + external crates,
  never on `app` or `cli`.
- `crates/cli` wires concrete adapters into `App` (`cli::wire`).

### Consequences of domain purity (patterns to follow)

- **Serde bridge types live in infra.** Domain structs carry no derives; each
  adapter defines local mirrors and converts: `infra-session`'s `SessionFile`
  (with a `version` tag), `domain::ExtraField` → `serde_json::Value` in
  `infra-telemetry`, LSP wire JSON → domain `LspLocation` in `infra-lsp`.
  Adding a field to a domain type means updating its mirror.
- **Errors** cross ports as `domain::BoxError` (`Box<dyn Error + Send + Sync>`).
  Adapters keep their own `thiserror` enums internally and box at the boundary.
- **All entity fields are owned** (`String`, `PathBuf`, `Box<[T]>`) so lifetimes
  never propagate through use-cases.

### No async runtime, anywhere

Ports are synchronous by design. `LlmPort::stream` returns
`Box<dyn Iterator<Item = Result<LlmEvent, BoxError>> + Send>` and the provider
clients use `reqwest::blocking`. The TUI runs the engine on one `std::thread`
with an `mpsc` channel to the renderer; `main` is a plain `fn`.

Do not reintroduce `#[tokio::main]`: a `reqwest::blocking` client cannot be
dropped inside a runtime context and panics with *"Cannot drop a runtime in a
context where blocking is not allowed"*. That bug shipped once already.

### Streaming

`infra-llm` decodes response bodies **incrementally** — `SseDecode`
implementations (`OpenAiDecoder`, `AnthropicDecoder`, `OllamaDecoder`) consume
one line at a time and are driven both by the live `EventStream` reader and by
the `parse_*_events(body)` batch helpers, so unit tests exercise the exact
production path. Terminal `Finish` events are held back until end-of-stream
because providers report token usage *after* the stop reason.

## Engine loop

`app::AgentLoop::execute` is the whole product: open/resume a session, render
history + mode-filtered tool specs, stream the model, dispatch tool calls,
truncate results before they enter the transcript, checkpoint, repeat until a
final answer or the turn cap. It also owns mode gating, `CancelFlag` /timeout
handling, and telemetry emission. Sessions are checkpointed by *moving* the
history in and out of the `Session` rather than cloning it.

## Tool namespace

Native: `read`, `write`, `str_replace_editor`, `apply_patch`, `list_dir`,
`shell`, `zcode_skill`. MCP: `mcp__<server>__<tool>`. LSP: `lsp__goto_definition`,
`lsp__find_references`, `lsp__hover`, `lsp__rename_symbol`.

`__` rather than `::` because provider function-calling APIs only accept
`[A-Za-z0-9_-]`. `domain::canonical_tool_name` maps the PRD spellings
(`mcp::srv::tool`, `zcode:skill`) onto the wire names, and both the registry
(dispatch) and `domain::modes` (planning-mode gating) go through it so they can
never disagree — a gate that missed an alias would be a security hole.

Adding a tool: implement `domain::Tool`, register it in
`ToolRegistry::from_config`, and if it mutates anything add it to
`domain::modes::execute_only_tool_names`.

Tool convention: a failure the *model* can fix (missing file, bad args, blocked
command, unapplied hunk) returns `Ok(ToolResult { error: Some(..) })` so the
loop feeds it back; only infrastructure failures return `Err`.

## Configuration

Layered, each overriding the previous field by field:

```
defaults → ~/.config/zcode/config.{json,toml} → <project>/zcode.{json,toml} → ZCODE_* env → flags
```

The project file is found by walking **up** from the current directory, so the
agent works from any subdirectory; `working_dir` then anchors to the directory
holding it. JSON wins over TOML in the same directory, and the nearest config
wins over one further up. `Loader::discover_from` is the testable entry point.

Secrets are referenced by env-var *name* via `api_key_env` and resolved at
wiring time — never read from the config file or written to disk. `model` and
`api_key_env` fall back to per-provider defaults
(`Provider::default_model` / `default_api_key_env`). `zcode config` prints the
resolved layers and effective values (never the key itself).

Skills are markdown notes discovered across three roots (project
`.zcode/skills`, configured `skills_dir`, machine-wide
`~/.config/zcode/skills`), in either `<name>.md` or `<name>/SKILL.md` layout.
`SkillIndex` builds the catalogue; `SkillTool`'s *description* lists the names
and summaries, which is what makes the model able to call it at all. The tool
is not registered when no skills exist.

Runtime state lives under `<working_dir>/.zcode/`: `sessions/<uuidv7>.json`,
`reports/<ts>-<session>.json`, `skills/`.

`shell_allowed` is a regex allowlist: the command is split on `;`/`|` and every
segment must match a pattern **in full** (patterns are anchored), and commands
containing `` ` ``, `$(`, `>`, `<`, `&` are refused outright — otherwise
`echo hi $(rm -rf /)` would pass an `echo .*` rule. An empty list denies
everything.

## Conventions

- `#![forbid(unsafe_code)]` everywhere; there is no `unsafe` in the workspace.
- Clippy warnings are errors; `rustfmt.toml` sets `max_width = 100`.
- Tests are inline `#[cfg(test)] mod tests`; there are no `tests/` dirs.
  Network- and binary-dependent tests are `#[ignore]`d so `cargo test
  --workspace` stays hermetic.
- Config tests that touch `ZCODE_*` env vars must take the module's `env_guard()`
  lock — env is process-global and parallel tests otherwise flake.
- Release profile uses `panic = "abort"`; avoid anything that relies on unwinding.

## Spec-driven workflow

Milestones live under `docs/prd/<milestone>/`: `prd.md` (numbered `FR-*`/`NFR-*`
requirements), `technical-plan.md` (`DQ1..DQ12` decisions), `tasks/task-NN-*.md`,
and `code-review.md`. Source comments cite these ids; when changing a cited
behaviour, read the requirement first and keep the citation accurate. Work ships
on `develop-release-<milestone>` branches merged via PR.

Where the implementation deliberately departs from a task doc — the tool-name
separator, the shell-allowlist matching rule, the absent async runtime — the
reason is recorded in `CHANGELOG.md` and in comments at the code in question.
