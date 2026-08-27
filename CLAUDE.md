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

## Modes

Three, laddered (`domain::AgentMode`): `planning` (read-only) → `editing`
(writes, no shell) → `auto` (everything). `build` parses as `auto` for
back-compat, and `infra-session`'s `SerializableMode` keeps a read-only `Build`
variant so v0.1 session files still load.

`domain::modes::denies(mode, name)` is the **single** authority: both
`App::tool_specs_for` (what the model is told about) and the dispatch gate in
`AgentLoop::execute` call it, so the advertised set can never disagree with the
permitted set. Names are canonicalised first.

## JSON output

Two schemas, chosen with `--json-format`. `zcode` (default) is the flat JSONL in
`infra-telemetry`. `opencode` is `infra-telemetry::opencode`, a translation onto
opencode's `session.next.*` envelopes, transcribed from its
`packages/schema/src/session-event.ts` — field names and types are theirs. It is
a *translation, not an emulation*: no durable block, no sequence numbers, no
`message.*`/`session.created`, because zcode has no message store or bus. `wire`
tees it with a sink-backed `JsonTelemetry` so the report file is still written.

## Cost

`domain::pricing` is a stdlib-only price table (USD per Mtok) with
longest-prefix matching over a normalised model id — vendor namespace stripped,
routing suffix stripped, `.` folded to `-` so `anthropic/claude-3.5-haiku`
matches `claude-3-5-haiku`. `cache_within_input` distinguishes OpenAI (cached
tokens counted inside `input_tokens`) from Anthropic (reported separately), so
the estimate is not double-counted. An unknown model yields `priced: false`
rendering as `n/a` — never a confident `$0.00`. Config `[[pricing]]` entries are
prepended; ties in prefix length go to the earlier entry, which is what makes an
override win.

## Retries

`infra-llm::send_with_retry` collects a `Vec<RetryNotice>` and returns it in
`RetriedResponse`; each `stream()` prepends them as `LlmEvent::Retry` before the
body's events. The app re-emits them as `UiEvent::Retry` + an `llm_retry`
telemetry event. It does **not** also log them — in the TUI the log stream is
rendered into the same timeline, so logging would double every retry.
`Retry-After` is honoured (integer, fractional, or HTTP-date), capped at
`MAX_BACKOFF` (120s). Without one, `RetryPolicy` picks the base by cause: a 429
starts at `rate_limit_backoff` (30s, config `rate_limit_backoff_ms`) because a
provider that just refused you is still refusing you 600ms later; anything else
starts at `TRANSIENT_BACKOFF` (500ms). Both double per attempt and carry
pid-derived jitter.

## TUI

`crates/cli/src/cli/tui/` — `mod.rs` (state + render loop), `timeline.rs` (the
entry model), `input.rs` (the prompt editor: byte offsets kept on char
boundaries), `wrap.rs`, `command.rs` (slash commands). Notes:

- **One pane.** There is no tools pane: `Timeline` holds one ordered list of
  `Entry` (user / agent / tool / note) and tool rows render inline under the
  message that made the call. `ToolCallStart` commits the streamed prose first,
  which is what puts the row *below* the sentence that announced it.
- **Failures wrap, successes clip.** A successful tool row is an index entry —
  the first line of the result, clipped to the row. A failure is the text the
  user has to act on, so `detail_wraps_below` sends it to `push_detail`, which
  wraps it in full underneath. `render_duration` steps ms → s → m → h so the
  right-aligned number always carries a unit.
- **Durations come from the engine.** `UiEvent::ToolResult.elapsed_ms` is
  measured in `AgentLoop` around the dispatch. The TUI cannot time it: events
  are drained in batches, so the gap between ingesting a start and a result is
  a fact about the channel — it read as `0ms` for a 20ms call.
- **Tabs never reach the renderer.** ratatui puts a tab in one cell; the
  terminal advances to the next tab stop. The two models disagree permanently
  and the screen is drawn over from that row on — which every Go file triggers.
  `timeline::expand_tabs` runs at ingest and on the streaming buffer.
- **One channel.** Engine events and the turn result share `EngineMsg`; two
  channels let a result overtake its own trailing deltas.
- **Wrap once, build only what shows.** `render_timeline` counts every entry's
  height with `wrap::height` (allocation-free) and builds rows only for
  entries intersecting the scroll window. `entry_height` and `entry_rows` must
  agree — `the_counted_height_always_matches_the_rows_built` pins it.
- **Memory.** `Timeline` is the one structure that grows with a session:
  `Box<str>` not `String`, every string capped at ingest (`MAX_TEXT`,
  `MAX_DETAIL`), `u32` timestamps, bounded at `MAX_ENTRIES`, and
  `heap_bytes()` exists so a test can hold it to a budget.
- **Bracketed paste.** `EnableBracketedPaste` + `Event::Paste`; without it a
  multi-line paste arrives as key events and every embedded newline sends.
- **Mouse capture, and the cost of it.** `EnableMouseCapture` is what makes the
  wheel scroll — the alternate screen has no terminal scrollback to fall back
  on, so without it the pane simply does not move. The price is that a plain
  drag no longer selects text (Shift, or Option on macOS Terminal, still does);
  `command::MOUSE_NOTE` says so in `/help`, because unexplained it reads as
  broken copy/paste.
- **Scrollback is clamped, not just saturated.** `draw_conversation` records
  `max_scroll` because only the draw knows the rendered height, and
  `scroll_up` stops there. Letting the counter run past the top is not
  harmless: the view stops while the number climbs, so the same number of
  scrolls back down does nothing — which reads as a pane that will not scroll.
- **Logging is redirected.** `cli::logging` installs one switchable logger;
  `LogRedirect` diverts records into the timeline as notes while the alternate
  screen is up, because stderr is the same terminal and a `log::warn!` paints
  over the UI. The guard restores stderr on every exit path.

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

## Providers

`Config.providers` is a `Box<[ProviderProfile]>` built from the file's
`providers` array; `Config.provider_name` is the label it was selected by and
`Config.provider` the `Provider` kind it resolves to. Profiles merge across
config layers **by name** (nearer layer replaces), so a machine-wide file can
declare the endpoints and each project only pick one.

`Config::select_provider(name)` is the single resolution point — the loader
calls it last, `--provider` calls it after load, and the TUI's
`Command::SwitchProvider` calls `with_provider` (a clone) so a failed build
leaves the running client alone. It looks `name` up in `providers` first, then
parses it as a built-in kind, so `--provider ollama` works with no profile
declared and a profile *named* `openrouter` shadows the built-in defaults.

A declared profile is **complete in itself**: what it omits comes from its
kind, never from top-level `model`/`api_key_env`/`base_url`. Those are the
single-provider form and apply only to a bare-kind selection. Inheriting them
across kinds produced an `api_key_env` that read `[set]` in `zcode config` and
then failed at the first request — a quiet wrong beats a loud wrong only if
you never have to debug it.

`App::set_llm` swaps just the client: the tool registry, and every MCP/LSP
child with it, keeps running, and so does the session — which is the point of
switching mid-conversation.

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

LSP is on by default. `Config::effective_lsp_servers()` merges configured
servers with `default_lsp_servers()` (rust-analyzer / gopls /
typescript-language-server), keeps a default only if `which_on_path` finds its
binary, and — when `detect_project_language` identifies the directory from its
marker files — starts **only** a server for that language. No marker means no
default server. `canonical_language` maps `nextjs`/`node`/`ts`/`golang` onto the
server they actually resolve to.

Runtime state lives under `<working_dir>/.zcode/`: `sessions/<uuidv7>.json`,
`reports/<ts>-<session>.json`, `skills/`.

Shell safety is three checks in `tools::guard`, in order:

1. **Structure** — `` ` ``, `$(`, `${`, `>`, `<`, `&` are refused outright, else
   `echo hi $(rm -rf /)` would pass an `echo .*` rule. Provably safe
   redirections (`2>&1`, `>/dev/null`, fd duplication among 0/1/2) are stripped
   *first* by `strip_safe_redirects`, so `go build ./... 2>&1` works.
2. **Denylist** (`DENIED_PATTERNS`) — irreversible/escalating/exfiltrating
   commands, refused **regardless of `shell_allowed`**. This is what lets the
   default allowlist be generous. `shell_denied` in config *extends* it and
   accumulates across config layers; nothing removes a built-in.
3. **Allowlist** — the command is split on `;`/`|`/newline and every segment
   must match a `shell_allowed` pattern **in full** (patterns are anchored). An
   empty list denies everything.

Checks 1 and 3 are skipped when `is_unrestricted` says a single pattern already
matches every command (`".*"` and friends, decided empirically against
`UNRESTRICTED_PROBES` — "does this regex accept everything?" is not a question
the regex crate answers). Structure exists to stop a *narrow* pattern being
widened by text the shell expands later; there is nothing to widen once
everything is allowed, and refusing `cd x && make` under `".*"` was a bug. The
denylist is never skipped — that is the invariant `shell_allowed` cannot touch.

`DEFAULT_SHELL_ALLOWED` lives in `infra-config` (it is a config default, and
the loader cannot depend on `tools`); `guard` re-exports it. It covers Go,
Rust, Node/TS, Python, and the common build tools, because the previous
`echo/ls/cd/cat` default made `go build` fail on a fresh install and taught
people to set `".*"`.

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
