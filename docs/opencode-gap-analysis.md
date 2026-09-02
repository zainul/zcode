# zcode vs opencode — gap analysis

**zcode** v0.2.0 (`79e3833`) · **opencode** v1.18.23 (`a0f36c9`, 2026-08-26)

zcode's stated goal is "OpenCode-equivalent core capabilities … with a
deliberately small memory and dependency footprint". This document measures
that claim: what is actually at parity, what is missing, and what zcode does
that opencode does not.

## How this was produced

Against opencode's source and its own published documentation, not from
memory or from its marketing copy:

- `packages/opencode/src/tool/registry.ts` — the tools actually registered
- `packages/opencode/src/cli/cmd/` — the CLI surface
- `packages/web/src/content/docs/*.mdx` — the user-facing feature docs
  (`tools`, `cli`, `config`, `permissions`, `agents`, `tui`, `keybinds`,
  `share`, `server`, `skills`, `providers`, `models`, `lsp`, `formatters`,
  `plugins`, `custom-tools`, `commands`, `themes`, `ide`, `rules`)

zcode's side is from its own binary — `zcode --help`, `zcode tools list`,
`zcode config` — and its source, not from this repository's own docs.

Anything below stated as a fact about opencode is traceable to one of those
files. Where a comparison is a judgement call rather than a fact, it says so.

---

## Scorecard

| Area | opencode | zcode | |
|------|----------|-------|---|
| Agent loop, streaming, tool calls | ✔ | ✔ | at parity |
| File read / write / edit / patch | ✔ | ✔ | at parity |
| Shell execution | ✔ | ✔ | zcode's guard is stricter |
| MCP servers | ✔ | ✔ | at parity |
| LSP | ✔ (experimental tool) | ✔ (4 tools, on by default) | zcode further |
| Skills (`SKILL.md`) | ✔ | ✔ | at parity |
| Sessions: create/continue/fork/import/export | ✔ | ✔ | at parity |
| Multiple providers in one config | ✔ | ✔ | at parity |
| Multimodal (image input) | ✔ | ✔ | at parity |
| Headless JSON output | ✔ | ✔ | zcode emits opencode's schema too |
| Cost/token accounting | ✔ (`stats`, ccusage) | ✔ (built-in price table) | different shape |
| **Context compaction** | ✔ | ✖ | **gap — highest impact** |
| **Per-tool permissions (ask/allow/deny)** | ✔ | partial (3 modes) | **gap** |
| **File access confined to the project** | ✔ (`external_directory`) | ✖ | **gap — verified** |
| **`.env` denied by default** | ✔ | ✖ | **gap — verified** |
| **Subagents / task delegation** | ✔ | ✖ | **gap** |
| **`grep` / `glob` tools** | ✔ | ✖ (shell only) | **gap** |
| **Undo / redo (snapshots)** | ✔ | ✖ | **gap** |
| **`todowrite` (task tracking)** | ✔ | ✖ | gap |
| **`webfetch` / `websearch`** | ✔ | ✖ | gap |
| **Interactive auth (`auth login`)** | ✔ | ✖ (env vars only) | gap |
| **Model catalogue (`models`)** | ✔ | ✖ | gap |
| **Custom commands** | ✔ | ✖ | gap |
| **Formatters** | ✔ (20+ built-in) | ✖ | gap |
| **Plugins / custom tools** | ✔ | ✖ (MCP only) | gap |
| **Server mode / SDK / ACP** | ✔ | ✖ | gap |
| **Session sharing (web links)** | ✔ | ✖ | out of scope |
| **Themes** | ✔ | ✖ | out of scope |
| **IDE extension** | ✔ | ✖ | out of scope |
| **Desktop / web client** | ✔ | ✖ | out of scope |
| Token-optimised shell output (rtk) | ✖ | ✔ | zcode further |
| Single static binary | ✖ (Bun runtime) | ✔ | zcode further |

---

## The gaps that matter

Ordered by how likely they are to stop real work.

### 1. No context compaction — sessions have a hard ceiling

opencode summarises the transcript when it approaches the model's context
window (`compaction.auto`, on by default, with `prune` and `reserved`
options), and exposes `/compact` for doing it on demand.

zcode has **no equivalent**. It bounds each *tool result*
(`max_tool_output_chars`) but never the transcript as a whole, and nothing in
the codebase knows what a model's context window is. A long session therefore
grows until the provider rejects the request, and the failure arrives as a raw
provider error mid-task.

This is the single largest gap. Everything else on this list makes zcode less
convenient; this one makes long sessions *end*.

The mitigations that exist — `/new`, `max_turns`, session fork — all require
the user to notice first.

### 2. Permissions are coarse

opencode gates each tool independently, with three actions (`allow`, `ask`,
`deny`) and pattern matching on the argument — `bash` matched against the
parsed command, `read` against the path, `webfetch` against the URL. It also
ships two behavioural guards: `external_directory` (a tool reaching outside
the project) and `doom_loop` (the same call repeated three times), both
defaulting to `ask`.

zcode has three laddered modes (`planning` → `editing` → `auto`) plus a shell
allowlist and denylist. That covers the common case well and the shell case
better than opencode does, but:

- there is no **ask** — every decision is made up front, and nothing can
  prompt mid-run;
- gating is per-*category*, not per-tool: you cannot allow `read` but deny
  `list_dir`;
- there is no `doom_loop` guard — a model repeating one failing call burns
  turns until `max_turns`.

Two findings here are worth separating from the rest of this document, because
they are not "opencode has a feature we lack" — they are zcode behaving in a
way most users would not expect. Both were **verified against the running
binary**, driving the real agent loop with a stubbed provider:

**The file tools are not confined to `working_dir`.** `tools::native::resolve`
joins a relative path onto the root but returns an absolute path unchanged, and
nothing rejects `..`. Reading `/etc/hosts` and `../../../../../../etc/hosts`
both succeed from a project directory, and `write`, `str_replace_editor`,
`apply_patch` and `list_dir` share the same resolver. opencode treats leaving
the project as `external_directory` and asks first.

**`.env` is readable.** opencode denies `*.env` by default. zcode has no such
rule, and a model asked to read one gets `SECRET_TOKEN=abc123` straight into
the transcript — which is then sent to the provider on every subsequent turn.

Neither is exploitable by an outside party on its own: the model chooses the
path. But "the model chose it" is exactly the threat model an agent has to
survive, and prompt injection from a file or a web page is the ordinary way it
fails.

### 3. No subagents

opencode's `task` tool spawns a subagent with its own context, and it ships
seven built-in agents (`build`, `plan`, `general`, `explore`, `scout`,
`compaction`, `title`). This is how it explores a large codebase without
filling the main transcript.

zcode has one agent and one transcript. Combined with gap 1, a broad "find
where X is handled" question costs main-context tokens it never gets back.

### 4. No `grep` or `glob` tools

opencode ships both, backed by ripgrep, with structured output.

zcode's model has to reach for `shell` and run `rg`/`find` itself. It usually
works — `rg` is in the default allowlist and rtk compacts the output — but it
depends on the tool being installed, the allowlist permitting it, and the model
spelling the invocation correctly. A first-class tool is more reliable and
cheaper to describe.

### 5. No undo

opencode snapshots the working tree during a run and offers `/undo` and
`/redo`.

zcode edits in place. `apply_patch` and `str_replace_editor` refuse a hunk that
does not match, so an edit built on a stale read fails rather than corrupting —
but once an edit lands, git is the only way back.

### 6. Smaller gaps

| Missing | Consequence |
|---------|-------------|
| `todowrite` | No structured plan for multi-step work; the model tracks progress in prose |
| `webfetch` / `websearch` | Cannot read a URL or look anything up; everything must be local or pasted |
| `auth login` | Keys are env vars only. Fine for CI, more friction on a laptop |
| `models` | No way to discover model ids; you must know the string |
| Custom commands | No project-defined `/command` shortcuts |
| Formatters | Edited files are not formatted; the model must run the formatter via shell |
| Plugins / custom tools | Extension is MCP-only. Enough for tools, not for hooks |
| Server / SDK / ACP | No embedding zcode in an editor or another program |

---

## Where zcode goes further

Not a longer list, but not an empty one.

- **Token-optimised shell output.** [rtk](https://github.com/rtk-ai/rtk) is on
  by default and installed if missing: `ls -la` returns 87% fewer bytes,
  `git status` 59%. opencode has no equivalent.
- **A stricter shell guard.** An always-on denylist that configuration cannot
  override, plus structural checks against smuggling. Recursive deletes are
  judged by their *target* rather than their flag, so `rm -rf node_modules`
  runs and `rm -rf ~` does not.
- **LSP on by default**, as four first-class tools, with the server chosen from
  the project's own marker files. opencode's LSP tool is experimental and
  behind a flag.
- **Cost estimation without a subscription.** A built-in price table with
  longest-prefix model matching, correct cache accounting per vendor, and an
  honest `n/a` for unknown models rather than a confident `$0.00`.
- **Both JSON schemas.** zcode emits its own flat JSONL *and* opencode's
  `session.next.*` envelopes, so tooling written against opencode works.
- **Footprint.** A single 5.2 MB static binary, 253 crates in the lockfile, no
  async runtime, no GC. opencode is a 32-package Bun/TypeScript monorepo
  requiring the Bun runtime, 194 MB installed. Running the same multi-file
  REST-endpoint task to a green build on the same model, zcode held a flat
  ~15 MB resident set against opencode's 420–590 MB — see the
  [memory benchmark](memory-benchmark.md).

---

## Deliberate divergences, not gaps

These differ on purpose and are recorded in `CHANGELOG.md` and in comments at
the code:

- **Tool namespace.** `mcp__server__tool`, not `mcp::server::tool` — provider
  function-calling APIs only accept `[A-Za-z0-9_-]`.
- **No async runtime.** Ports are synchronous; the TUI runs the engine on one
  `std::thread`. A `reqwest::blocking` client cannot be dropped inside a
  runtime context.
- **The opencode JSON schema is a translation, not an emulation.** No durable
  block, no sequence numbers, no `message.*` or `session.created`, because
  zcode has no message store or bus.
- **Three modes, not opencode's agent system.** A ladder is easier to reason
  about than a permission matrix, at the cost of granularity (gap 2).

---

## If the goal is parity

Highest value first, judged by what stops work rather than by effort:

1. **Context compaction.** Track token usage against a per-model context
   window; summarise the oldest turns when it approaches. Removes the hard
   ceiling on session length.
2. **Confine the file tools to `working_dir`, and deny `.env` by default.**
   Both are small and both close a real hole — see gap 2. Per-tool
   `ask`/`allow`/`deny` is the larger design that follows.
3. **`grep` and `glob` tools.** Small, self-contained, and they remove a
   dependency on the model's shell spelling.
4. **A `doom_loop` guard.** Cheap — the engine already sees every call — and
   it stops the most common way a run wastes a turn budget.
5. **Subagents.** Large, and worth much less before compaction exists.

Sharing, themes, the desktop client and the IDE extension are listed above as
out of scope rather than as gaps: they are product surface around the agent
rather than agent capability, and none is implied by "OpenCode-equivalent core
capabilities".
