# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — several providers in one config

`providers` is an array of named endpoints; `provider` says which is active:

```json
{
  "provider": "free",
  "providers": [
    { "name": "free", "kind": "openrouter", "model": "poolside/laguna-s-2.1:free" },
    { "name": "fast", "kind": "openrouter", "model": "anthropic/claude-haiku-4.5" },
    { "name": "gateway", "kind": "openai-compatible", "model": "internal-1",
      "base_url": "https://gateway.internal/v1/chat/completions" },
    { "name": "local", "kind": "ollama" }
  ]
}
```

Each entry carries its own `model`, `api_key_env` and `base_url`, so an
endpoint is described in one place instead of four top-level keys that have to
be changed together. `name` and `kind` each default to the other, which makes
the one-line form work: an entry named after a built-in provider overrides that
provider's defaults, and is how you give OpenRouter — or any built-in — a URL
of your own.

Switch with `--provider NAME` for a run, or `/provider NAME` in the TUI, which
now also lists what is configured. A TUI switch replaces **only** the model
client: the conversation, the session, and every MCP and LSP child process stay
as they were, so you can start on a cheap model and finish on a better one
without losing the context that got you there. The new client is built before
the old one is dropped, so a bad name or a missing key reports itself and
leaves the working provider in place.

`zcode config` lists every endpoint with the active one marked, and an unknown
name says what it would have accepted rather than only what it did not.

**A declared profile is complete in itself.** What it omits comes from the
defaults for its `kind`, never from a top-level `model` / `api_key_env` /
`base_url` — those were written for whichever provider the config had at the
time, and letting an unrelated gateway inherit a key variable produces one that
reads `[set]` in `zcode config` and then fails on the first request. Top-level
keys still apply to a provider selected as a bare kind, so every config written
before this change behaves exactly as it did.

### Fixed — the conversation now scrolls with the mouse

The wheel did nothing. zcode never asked the terminal to report mouse events,
and the alternate screen has no scrollback of its own to fall back on, so the
pane simply did not move — `PageUp` worked, but nobody reaches for `PageUp`
first.

`EnableMouseCapture` fixes that (three rows a notch, as terminals themselves
scroll). `Ctrl-Home` / `Ctrl-End` jump to either end.

Scrolling up also **stopped at the top rather than counting past it**. The
offset saturated but the counter did not, so scrolling up a hundred notches in
a pane forty rows deep banked sixty invisible ones — and then swallowed the
first sixty scrolls back down. That is indistinguishable from a pane that will
not scroll at all, and was most of the reason this looked broken. The renderer
now records how far back the conversation actually goes, and the wheel and keys
clamp to it.

The cost of mouse capture is that a plain drag no longer selects text. Every
terminal keeps that on a modifier — Shift, or Option on macOS Terminal — and
`/help` now says so, because unexplained it reads as broken copy/paste.

### Fixed — `"shell_allowed": [".*"]` now actually allows everything

`cd /workspace && go build ./... 2>&1 | head` was refused with *command blocked
by the shell allowlist* even when `shell_allowed` was `[".*"]`. The structure
check — which refuses `` ` ``, `$(`, `${`, `>`, `<`, `&` — ran ahead of the
allowlist and unconditionally, so a config that permitted every command still
could not run one containing `&&` or a pipe.

The structure check exists to stop a *narrow* pattern being widened by text the
shell expands later: `echo hi $(rm -rf /)` matches `echo .*`, but what runs is
not what was checked. That reasoning does not apply once every command is
allowed — whatever the shell expands to is allowed too. So `guard::is_allowed`
now skips both the structure check and the segment split when
`guard::is_unrestricted` finds a pattern that already matches everything.

"Does this regex accept every string?" is not a question the regex crate
answers, so it is decided empirically: a pattern counts as unrestricted when it
matches all of `UNRESTRICTED_PROBES`, a set carrying every construct the
structure check exists to refuse. `".*"`, `"(?s).*"`, `"[\s\S]*"` and `".+"`
all qualify.

**The denylist is unchanged and still not overridable.** `rm -rf /`,
`curl … | sh` and the rest are refused under `[".*"]` exactly as before — that
is the invariant `shell_allowed` cannot touch. An empty allowlist still denies
everything, and an unrestricted one still requires a non-empty command.

`zcode config` now names the state, so an open allowlist is never a surprise:

```
shell_allowed          1 pattern(s) — unrestricted: anything the denylist
                                      permits, pipes and `&&` included
shell_denied           23 built-in + 0 from config
```

A command refused for its structure under a narrow allowlist now says so, and
points at the escape hatch instead of leaving you to guess:

```
  hint: shell metacharacters (`$(`, backticks, `>`, `<`, `&&`) are not allowed
        under a narrow allowlist; only `2>&1` and `>/dev/null` are. Run the
        command without them, or set `shell_allowed` to [".*"], which permits
        every command the built-in denylist does not refuse.
```

### Fixed — a tool failure is no longer truncated to fit its row

The message that mattered most was the one you could not read:

```
  └ ✖ 20:24:31  shell              command blocked by the shell allowlist (`sh…
```

Clipped there it names neither the rule that refused the command nor the
command itself — the two things you need to fix it.

Tool rows now clip only *successes*, which are index entries (the first line of
what came back) and are not what you are scanning for. A failure or a refusal
that does not fit is wrapped in full underneath its row:

```
  └ ✖ 20:24:31  shell                                               1.2s
      command blocked by the shell allowlist (`shell_allowed` in
      zcode.json/zcode.toml): cd /workspace && go build ./... 2>&1 | head
        hint: no pattern in `shell_allowed` matches `cd`; add one, e.g.
        "cd( .*)?"
```

Short failures stay on their row, so the extra lines are only spent when there
is no alternative. `MAX_DETAIL` rises from 200 to 400 characters so the guard's
refusal survives ingest with its hint attached — at 400 entries that still
bounds tool rows to ~160 KB, and the saturated-timeline budget test is
unchanged.

### Fixed — tool durations were measured in the wrong place, and stopped at minutes

Two problems with the number on the right of a tool row.

It was **timed by the UI**, from the interval between ingesting a
`ToolCallStart` and ingesting its `ToolResult`. Events are drained from the
channel in batches, so that interval describes the channel, not the tool: a
call the engine measured at 20ms rendered as `0ms`, which `render_duration`
draws as nothing at all. Against a live provider the gaps happened to be wide
enough to look plausible, which is worse — the number was believable and wrong.

`UiEvent::ToolResult` now carries `elapsed_ms`, measured in `AgentLoop` around
the dispatch itself, and the timeline uses it verbatim. The same figure is
emitted as `duration_ms` on the `tool_result` telemetry event. (The opencode
translation is unchanged: `session.next.tool.success` has no such field, and
inventing one would make it an emulation rather than a translation.)

And `render_duration` stopped at minutes, so an hour-long call read `73m20s`.
It now steps `82ms` → `1.2s` → `2m05s` → `1h25m`, carrying a unit at every
magnitude a `u32` of milliseconds can reach.

### Changed — the TUI is one timeline, not two panes

Tool calls now appear inline, under the message that made them, instead of in a
separate pane you had to correlate by eye:

```
20:21:35  zcode
  I'll read main.go and list the directory for you.
  tools used
  ├ ✔ 20:21:35  read               package main
  └ ✔ 20:21:35  list_dir           .zcode/
```

Every block carries a local wall-clock timestamp, every tool row a status icon
(`◐` running, `✔` ok, `✖` failed, `⊘` refused), what it acted on, and how long
it took. A run of calls is labelled and bracketed. A result settles its own row
rather than adding a second one, so a call is always exactly one line.

Engine notes are inline too, with their own markers: `↻` retry, `!` warning,
`·` information.

Dropping the tools pane gives the conversation eight more rows and removes the
parallel copy of every tool line from memory.

### Changed — rate limits back off for 30 seconds, not 600ms

A 429 was retried on the same fast exponential curve as a dropped connection —
0.6s, 1.2s, 2.2s — which on a busy or free tier failed all three times and
turned a recoverable pause into a failed run.

Backoff now depends on *why* we are retrying:

| Cause | Wait |
|-------|------|
| 429 with `Retry-After` | exactly what the provider asked |
| 429 without one | a flat **30s** (`rate_limit_backoff_ms`) |
| 5xx, timeout, dropped connection | 500ms, doubling |

The rate-limit wait is deliberately flat. The window a provider meters over is
fixed, so doubling does not improve the odds — it only turns a recoverable
pause into minutes of silence. The worst case stays predictable at
`max_retries × rate_limit_backoff_ms`, 90 seconds at the defaults. Transient
errors still back off progressively.

Both are jittered and capped at 120 seconds. The provider's own header always
wins. Verified against a live OpenRouter free route, where the old curve failed
every time.

### Added — opencode-compatible JSON output

`--json-format opencode` emits opencode's `session.next.*` event envelopes
instead of zcode's flat JSONL:

```json
{ "id": "evt_000000000002",
  "type": "session.next.tool.called",
  "data": { "timestamp": 1787749129711, "sessionID": "ses_…", "callID": "c1",
            "tool": "read", "input": { "path": "main.go" },
            "provider": { "executed": false } } }
```

The schema is transcribed from opencode's own
`packages/schema/src/session-event.ts`, not approximated: `model` is a
`{ id, providerID }` ref, `input` is an object, tokens use the nested
`{ input, output, reasoning, cache: { read, write } }` shape, and `callID`
correlates a call with its `tool.success` / `tool.failed`.

It is a **translation, not an emulation**, and the gaps are deliberate: zcode
has no message store or event bus, so there is no `durable` block, no sequence
numbers, and no `message.*` / `session.created` / `permission.*`. A client that
*reads the stream* will work; one that *synchronises state* by diffing whole
entities needs opencode itself. `docs/guide/15-events.md` says so plainly.

The run report is written either way.

### Fixed — tab-indented files corrupted the screen

Reading any Go file drew the interface over itself, permanently. ratatui writes
a tab into one terminal cell; the terminal advances the cursor to the next tab
stop. The two models disagree from that row on, and the diff renderer never
recovers. Tabs are now expanded to spaces at ingest and on the streaming
buffer, so they never reach the renderer.

### Fixed — the status bar dropped the cost on a narrow terminal

A long model id (`openrouter/poolside/laguna-s-2.1:free` is 39 characters)
pushed the cost off the right edge at 100 columns — the one field you cannot
recompute by looking at the screen. The bar now sheds detail to fit: the vendor
namespace first, then the model name, then the cache count, then the token
totals. State, mode, and cost always survive. Tested from 40 columns up.

### Fixed — a free route showed `n/a` until the first turn

The TUI seeded its cost from a price-table lookup, which an OpenRouter `:free`
route has no entry in — so it read `n/a` at startup and `$0.00` after the first
turn. Both paths now ask `PriceTable::knows`, which understands free routes.
`zcode config` reports them as "free route" rather than unpriced.

### Changed — memory

The timeline is the one structure that grows with a session, so it is built to
stay bounded:

- Text is stored as `Box<str>`, not `String`: no capacity field, no growth
  slack. A `String` built by `push_str` typically carries ~2x its length.
- **Every string is capped at ingest.** Previously only the *line count* was
  bounded, so a tool returning one 100 KB line retained all of it.
- Timestamps are `u32` seconds from session start (4 bytes) rather than
  `SystemTime` (16); durations are `u32` milliseconds.
- `/clear` and `/new` release the backing allocation instead of keeping its
  capacity.
- A large streamed answer no longer holds its peak buffer for the rest of the
  session.
- The renderer counts row heights without allocating (`wrap::height`) and
  builds only the rows inside the scroll window — previously every row of a
  400-entry timeline was materialised twelve times a second to show thirty.

`Timeline::heap_bytes()` exposes the accounting, and tests hold a saturated
timeline under 1 MB. Measured: 6.6 MB idle RSS, 7.6 MB after sustained churn.


### Added — a third agent mode

`editing` sits between `planning` and the mode formerly called `build`, now
`auto`. It may write files but not run shell commands, because *"may rewrite my
source"* and *"may execute arbitrary commands"* are different grants of trust:
an edit leaves a reviewable diff, a command does not.

`build` still parses as `auto` everywhere, and session files written by v0.1
still load. `domain::modes::denies` is now the single authority for gating —
both the tool-spec filter and the dispatch gate call it, so the set the model is
shown can never disagree with the set it may use.

### Added — cost estimates

`domain::pricing` carries published list prices for the common models. The TUI
shows a running total, `zcode run` prints one in its summary line, and `finish`
events and report files carry `cost_usd`.

It is an estimate, not a bill. An unknown model reports `n/a` rather than a
confident `$0.00`, and `cache_within_input` distinguishes OpenAI's accounting
(cached tokens inside `input_tokens`) from Anthropic's (reported separately) so
the figure is not double-counted. Override or extend the table with
`[[pricing]]` in the config.

### Added — language servers on by default

zcode now identifies the project from its marker files (`go.mod`,
`Cargo.toml`, `tsconfig.json`/`next.config.*`, `package.json`) and starts the
matching language server — gopls, rust-analyzer, or
typescript-language-server — if that binary is on `PATH`. Next.js is not a
separate entry: it is a TypeScript project, and the aliases `nextjs`, `next`,
`node`, `ts`, `tsx`, `golang` resolve accordingly.

Only a server for *this* project's language is started: a Go repo on a machine
that also has rust-analyzer installed does not get rust-analyzer. A directory
with no marker at all starts no default server. `lsp.defaults = false` opts out;
naming a server for a language replaces its default.

MCP and LSP are configured in the same file as everything else — there was never
a separate config, and the docs now say so plainly.

### Added — TUI slash commands, and a usable prompt

`/help`, `/exit` (`/quit`, `/q`), `/mode`, `/cost`, `/model`, `/session`,
`/tools`, `/new`, `/clear`, `/stop`. An unrecognised `/word` is reported rather
than sent to the model as a prompt; a path like `/usr/local/bin` still reaches
it.

The prompt is now a real editor: a **visible caret**, arrow and word motion,
`Home`/`End`, `Ctrl-A`/`E`/`W`/`U`/`K`, `Alt-Enter` for a newline, and a box
that grows with its content. `Shift-Tab` cycles mode; `PageUp`/`PageDown`
scroll.

### Fixed — the TUI

- **Paste was truncated.** Bracketed paste is now enabled, so the whole
  clipboard arrives at the caret in one piece. Previously a multi-line paste
  arrived as a burst of key events, which dropped characters under load and
  sent the prompt on the first embedded newline.
- **No cursor was drawn.** There is one now, and it tracks the caret across
  wrapped rows and explicit newlines.
- **Long text did not scroll correctly.** The scroll offset was computed from
  logical lines while the widget wrapped at draw time, so a long answer scrolled
  past its own tail. Wrapping now happens once, before rendering, and the offset
  is computed from the same rows that are drawn.
- **Progress was invisible.** The status bar now carries a spinner, the step
  count, an elapsed clock, the mode, provider/model, running token totals, and
  the estimated cost. A provider failure turns it red and stays until the next
  turn.
- **Log output painted over the interface.** `env_logger` writes to stderr,
  which under the alternate screen is the same terminal — a warning from a
  failing MCP or LSP server corrupted the display. `cli::logging` now installs a
  switchable logger that diverts records into the tools pane while the TUI is
  up, and restores stderr on every exit path.
- **A turn result could overtake its own trailing output.** Events and results
  travelled on separate channels; they now share one.

### Fixed — `go build ./... 2>&1` was refused

`2>&1` contains `&`, and the guard refused every command containing shell
metacharacters. Redirections that cannot introduce a command or write to a real
file — fd duplication among 0/1/2, and `/dev/null` — are now stripped before
that check. `echo hi > /etc/passwd 2>&1` is still refused.

The default allowlist was also `echo`, `ls`, `cd`, `cat`, which meant a fresh
install could not build anything, and the fix everyone reached for was
`"shell_allowed": ["*"]` — switching the safety net off. The default now covers
Go, Rust, Node/TypeScript, Python, and the common build tools, paired with a new
**denylist** that `shell_allowed` cannot override: `rm -rf`, `sudo`, `dd of=`,
`mkfs`, fetch-and-run pipelines, `shutdown`, `git push --force`,
`git reset --hard`, `npm publish`, and reads of `~/.ssh/id_*` or
`~/.aws/credentials`. `shell_denied` extends it and accumulates across config
layers, so a machine-wide ban cannot be dropped by a project file.

A blocked command now explains itself, so the model fixes the command instead of
retrying it verbatim.

### Fixed — rate limits looked like hangs

429s were already retried, but silently: the agent simply stopped for several
seconds. Retries are now first-class events (`LlmEvent::Retry` →
`UiEvent::Retry` → the `llm_retry` telemetry kind), rendered as
`↻ rate limited by the provider (429) — retrying in 2.0s (attempt 1/3)` in both
interfaces.

`Retry-After` is honoured in all three spellings the header allows (integer
seconds, fractional seconds, HTTP-date) and capped at 60s so a hostile or
mistaken value cannot park the agent. Backoff otherwise grows exponentially with
process-derived jitter, so two agents throttled at the same instant do not
return at the same instant. The default budget rose from 2 to 3 retries and is
configurable with `max_retries`.

### Fixed

- **`base_url` was doubled.** `openai-compatible` and `vllm` appended
  `/chat/completions` unconditionally, so a `base_url` copied from a curl
  example produced `…/v1/chat/completions/chat/completions` and a 404 naming a
  URL the user never typed. Either spelling is now accepted.
- **OpenRouter model ids went unpriced.** The price table only carried
  Anthropic's dashed spelling, so `anthropic/claude-3.5-haiku` showed `n/a`.
  Version dots and dashes are now folded together. An OpenRouter `:free` route
  is recognised as free.
- **A real charge rendered as `$0.00`.** Sub-$0.0001 totals now show
  `<$0.0001`; `$0.00` means free.

### Added — documentation

Two new chapters: [Command reference](docs/guide/14-commands.md) — every
subcommand, flag, slash command, key, and environment variable — and
[Event reference](docs/guide/15-events.md), which specifies every `--json` event
and states plainly that the schema is **not** 1:1 with opencode's, why, and what
matching it would involve.

`docs/guide/05-tui.md` is now illustrated with real screen captures, produced by
driving the binary on a pseudo-terminal.

### Added — acceptance tooling

`examples/` holds the harness used to verify the above against a live provider:

- `run-acceptance.sh` — 28 checks over the CLI surface, agent modes, shell
  safety, JSON output, and sessions, capturing real output to
  `examples/captures/`.
- `tui-screenshot.py` — drives the TUI on a pty and renders the escape-sequence
  stream, so a capture is the screen rather than a log. 38 checks.
- `fake-provider.py` — an OpenAI-compatible endpoint that answers the first N
  requests with 429, so the retry path is testable on demand.


### Changed — renamed to `zcode`

The project, binary, and every user-facing identifier moved from `ag` to
`zcode`. This is a breaking change for existing installs:

| Was | Now |
|-----|-----|
| `ag` binary and cargo package | `zcode` |
| `ag.json` / `ag.toml` | `zcode.json` / `zcode.toml` |
| `AG_*` environment variables | `ZCODE_*` (e.g. `ZCODE_OPENROUTER_API_KEY`) |
| `.ag/` state directory | `.zcode/` |
| `ag_skill` tool (alias `ag:skill`) | `zcode_skill` (alias `zcode:skill`) |
| `ag-benches` crate | `zcode-benches` |

To migrate an existing project: `mv ag.json zcode.json`, `mv .ag .zcode`, and
re-export your key under the new variable name.

Outbound identity changed with it: the HTTP `User-Agent` is now
`zcode/<version>`, and the MCP/LSP `clientInfo.name` and OpenRouter `X-Title`
report `zcode`.

### Added

- **Skills are discoverable by the model.** The `zcode_skill` tool description
  now lists every available skill with a one-line summary, so the agent can
  choose one unprompted. Previously it was told only "load a skill from the
  skills directory" and had to guess a filename, so in practice skills were
  never used.
- **`<name>/SKILL.md` layout supported** alongside `<name>.md`. A library using
  the Agent Skills convention was previously invisible: only top-level `*.md`
  was scanned.
- **Skills are searched across several roots** — the project's
  `.zcode/skills/`, any configured `skills_dir`, and the machine-wide
  `~/.config/zcode/skills/`. `skills_dir` now *adds* a root instead of
  replacing the project's, which had made per-project skills impossible
  whenever a global library was configured. Nearer roots shadow further ones.
- Skill summaries come from YAML front-matter `description:` when present,
  otherwise the first line of prose.
- `zcode skills list` shows the directories searched, each skill with its
  summary, and — when nothing is found — how to create one.

### Changed

- `zcode_skill` is no longer gated by planning mode. Loading a markdown note is
  read-only, and planning is exactly when house conventions should inform the
  proposal.
- The skill tool is not registered at all when no skills exist, so it costs no
  prompt budget and cannot invite a guessed name.
- An unknown skill name lists the available ones instead of failing opaquely.

### Added

- `scripts/update.sh`, and `install.sh` is now update-aware: it replaces an
  existing installation **in place** rather than installing a second copy to a
  different default prefix, prints the old and new build stamps, and warns when
  another copy earlier on `PATH` would shadow the one just installed.
- `zcode version` includes a build timestamp and marks a dirty working tree
  (`git: 9a99381-dirty, built: 2026-08-26T03:31:35Z`). Between releases the
  crate version and commit are identical, so without it there was no way to
  tell a stale installed binary from a fresh build — the cause of
  "`zcode config` says unrecognized subcommand".
- `zcode config` reports **problems** as well as values — an invalid
  `shell_allowed` regex, a missing API key variable, a `skills_dir` that does
  not exist — and exits non-zero, so it works as a CI preflight check.
- `~` is expanded in `working_dir` and `skills_dir`. A config saying
  `"skills_dir": "~/.config/zcode/skills"` previously resolved to a literal
  `./~/...` that never existed.
- An invalid `shell_allowed` pattern now reports the regex error and a hint.
  `"*"` (shell-glob thinking) failed every run with only
  `invalid shell_allowed pattern: *`; it now explains that these are regular
  expressions and suggests `.*`.
- **Layered configuration discovery.** A user-level
  `~/.config/zcode/config.{json,toml}` (honouring `$XDG_CONFIG_HOME`) now sits
  under the project config, so provider and key settings can be set once per
  machine. The project file is found by walking **up** from the current
  directory rather than only checking it, and `working_dir` anchors to the
  directory holding it — previously, running from a subdirectory silently fell
  back to built-in defaults with no warning.
- `zcode config` — prints which files were read, the full search path, and the
  effective settings, reporting only whether the API key variable resolves and
  never its value.
- `scripts/install.sh` — detects platform and architecture, builds the release
  binary, installs to `/usr/local/bin` or `~/.local/bin`, and prints the exact
  `PATH` line for the user's shell. Supports `--prefix`, `--no-build`,
  `ZCODE_INSTALL_DIR`. POSIX `sh`, so it runs under bash/zsh/dash/ash and in
  Git Bash, MSYS2 and WSL.
- `scripts/uninstall.sh` — finds every install on `PATH` plus the usual
  locations, confirms before deleting, and refuses to run unattended without
  `--yes`. Project data (`.zcode/`, `zcode.json`) is deliberately left alone
  and its location reported.
- `docs/guide/` — a 14-chapter user guide covering installation through MCP,
  LSP, multimodal input, and telemetry.

## [0.2.0] - 2026-08-24

### Added — core capability milestone

- **Agent loop** (`app::AgentLoop::execute`): streams the model, dispatches
  tool calls, feeds results back, checkpoints each round, and stops on the
  turn/token caps (FR-LOOP-01..04).
- **Two interfaces on one engine**: headless `zcode run` (with `--json` JSONL) and
  an interactive ratatui TUI (`zcode`/`zcode repl`) (FR-IFACE-01..06).
- **Tool registry** (`crates/tools`): native `read`, `write`,
  `str_replace_editor`, `list_dir`, `shell`, `zcode_skill`, merged with MCP and
  LSP tools into one namespace (FR-TOOL-*, DQ10).
- **Shell allowlist**: `GuardedShell` checks every command segment against the
  configured regexes before execution, and refuses substitution/redirection
  outright; an empty list denies everything (FR-CONFIG-04/05, NFR-SEC-02).
- **MCP client** (`crates/infra/mcp`): stdio JSON-RPC with deadlines; servers
  that fail to start are logged and skipped (FR-MCP-01..05).
- **LSP client** (`crates/infra/lsp`): stdio JSON-RPC with `Content-Length`
  framing, document mirroring, and `didChange` after edits (FR-LSP-01..04).
- **Session subcommands**: `create`, `continue`, `fork`, `import`, `export`
  (FR-SESSION-01..05).
- **Agent modes**: planning mode withholds every editing tool from the model
  and refuses one if requested anyway (FR-MODE-01..04).
- **Provider dispatch** in `wire()` for OpenAI, Anthropic, OpenRouter, Ollama,
  and vLLM/OpenAI-compatible endpoints (FR-MODEL-01..06).
- **Vision input**: `--image` files are base64-encoded into the request
  (FR-MODEL-08).
- `domain::canonical_tool_name`: `mcp::srv::tool` / `zcode:skill` map to
  provider-legal `mcp__srv__tool` / `zcode_skill`, with the `::` spellings kept as
  aliases so dispatch and mode-gating can never disagree.
- `make check-arch` and `make secrets-scan`.

- **Real incremental streaming**: response bodies are decoded line by line as
  they arrive, so the first token appears immediately instead of after the
  whole generation. Decoding lives in `SseDecode` implementations shared by the
  live reader and the batch parsers, so tests exercise the production path.
- **`apply_patch` tool**: unified diffs across multiple files, including
  creation and deletion. Hunks are located by context rather than by trusting
  `@@` line numbers, and nothing is written unless every hunk applies.
- **DeepSeek provider**, plus per-provider defaults for `model` and
  `api_key_env` so switching provider is a one-line change.
- **JSON configuration** (`zcode.json`), preferred over `zcode.toml` when both exist.
- HTTP hardening: retries with exponential backoff and `Retry-After` support
  for 429/5xx, immediate failure with the provider's own message for auth and
  model errors, and OpenRouter attribution headers.
- `timeout_ms` now reaches the provider clients instead of a hardcoded 30s.

### Changed

- `zcode session fork --as <name>` accepts any short, filesystem-safe name
  (letters, digits, `-`, `_`, `.`; max 64 chars). It previously demanded a
  UUIDv7, which no one can produce by hand, so the flag was unusable.
  Generated ids are still UUIDv7; traversal (`..`, separators, leading dots)
  is still refused.
- `mcp` and `lsp` are cargo features of the `zcode` binary, forwarded to `tools`,
  so `--no-default-features` genuinely drops those adapters. Previously the
  flag was silently ineffective.
- Tool results report paths relative to the working directory instead of
  repeating a long absolute prefix on every line.
- `--help` and `--version` exit 0 and print to stdout instead of being routed
  through the error handler with an `ag:` prefix and exit 1. Usage errors now
  exit 2.
- Help text no longer leaks internal requirement ids (`FR-IFACE-01`) into the
  product's `--help` output.

### Fixed

- Anthropic conversations with tool calls were malformed: the system prompt was
  sent as a user message, `tool_use` blocks were not emitted on assistant
  turns, and tool results used an invalid shape. Multi-turn tool use against
  Anthropic could not have worked.
- Anthropic cache accounting picked one usage field where writes and reads are
  reported separately; they are now summed.
- Ollama tool calls whose `arguments` are a JSON object (its actual wire shape,
  not OpenAI's string) were dropped.
- OpenAI streams `usage` in a trailing chunk *after* `finish_reason`; those
  token counts were dropped, so every run reported zero and silently fell back
  to the estimate heuristic. They are now folded into the finish event.
- The CLI ran its synchronous engine inside a `#[tokio::main]` runtime, which
  made `reqwest::blocking` panic on drop ("Cannot drop a runtime in a context
  where blocking is not allowed") — every real LLM call aborted. The runtime is
  gone; nothing in the agent is async.
- `UuidSessionStore::checkpoint` now stamps `last_message_at`.

### Changed

- `App` owns its ports as `Box<dyn Port + Send>` and exposes `AgentLoop`
  instead of the v0.1 `TaskRunner`/`EditPlanner` stubs (DQ5).
- `crates/cli` no longer depends on `tokio` or on `crossterm` directly
  (crossterm comes through ratatui's re-export, so the versions cannot skew).

### Added

- Workspace scaffolding for the zcode Rust coding agent.
- Domain crate with entities (`Task`, `FileEdit`, `ShellCommand`, `Plugin`, `AgentContext`), `DomainError`, and port traits.
- Application crate with `App` orchestrator and `TaskRunner`/`EditPlanner` use-case traits.
- Infrastructure crates: LLM adapter (stub), Filesystem adapter (std::fs), Shell adapter (std::process), Configuration loader (TOML + env).
- CLI crate with `version` subcommand, composition root (`wire()`), and single-threaded tokio runtime.
- Criterion smoke benchmark for domain entity construction.
- `Makefile` quality-gate runner with `ci`, `test`, `lint`, `fmt`, `bench`, `check-deps` targets.
- Documentation scaffold (README, architecture guide, CHANGELOG, CONTRIBUTING).

## [0.1.0] - 2026-08-24

### Scaffolding

- Initial project scaffolding milestone complete.
- No LLM calls, no chat loop, no PTY sessions — pure structural foundation.
- All quality gates green: `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`, `cargo doc`.
