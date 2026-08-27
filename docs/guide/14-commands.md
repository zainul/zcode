# 14. Command reference

← [Troubleshooting](13-troubleshooting.md) · [Index](README.md) · Next: [Event reference](15-events.md)

Every command zcode accepts, in one place: the CLI subcommands, their flags,
the TUI's slash commands, and the keys. Earlier chapters explain *why*; this
one is the lookup table.

Anything in `<angle brackets>` is a value you substitute. `[square brackets]`
mark an optional argument.

---

## CLI at a glance

| Command | What it does |
|---------|--------------|
| `zcode` | Open the interactive TUI (no subcommand) |
| `zcode run <PROMPT>` | Run one task headlessly and exit |
| `zcode repl` | Open the TUI, explicitly |
| `zcode session <SUB>` | Create, continue, fork, import, export sessions |
| `zcode config` | Show where configuration comes from and what it resolves to |
| `zcode tools list` | List the tools the model can call |
| `zcode skills list` | List the markdown skills it can load |
| `zcode version` | Version, git commit, build time, profile |
| `zcode help [CMD]` | Help for zcode or one subcommand |

Global exit codes:

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | The run failed (provider error, tool error, bad config) |
| `2` | Usage error — a bad flag or a missing argument |
| `130` | Interrupted with Ctrl-C; the session was checkpointed |

---

## `zcode run` — one task, then exit

```sh
zcode run <PROMPT> [OPTIONS]
```

The prompt is a positional argument; quote it.

| Flag | Value | Default | What it does |
|------|-------|---------|--------------|
| `--mode` | `planning` \| `editing` \| `auto` | from config | What the agent is allowed to do — see [chapter 8](08-agent-modes.md) |
| `--provider` | name or kind | from config | Which endpoint to use — see [chapter 12](12-configuration-reference.md#multiple-providers) |
| `--session` | session id | new session | Resume an existing session so its context carries over |
| `--json` | — | off | Emit one JSON object per event to stdout (JSONL) instead of prose |
| `--json-format` | `zcode` \| `opencode` | `zcode` | Event schema for `--json` — see [chapter 15](15-events.md) |
| `--image` | file path | — | Attach an image for a vision model. Repeatable |
| `--config` | file path | discovered | Use this config file instead of the discovered one |
| `--timeout` | seconds | from config | Give up after this long and checkpoint the session |

```sh
# The common case
zcode run "add doc comments to the public functions in src/lib.rs"

# Read-only: propose, do not touch anything
zcode run --mode planning "how should we split this module?"

# Continue that conversation with permission to edit
zcode run --session 019… --mode auto "do it"

# Machine-readable, for CI
zcode run --json "run the tests and fix what fails" > events.jsonl

# Vision
zcode run --image screenshot.png "why is this layout broken?"
```

Without `--json`, the model's answer goes to **stdout** and the summary line to
**stderr**, so `zcode run … > answer.txt` captures only the answer:

```
[3 step(s) · 4182 in / 220 out / 0 cached tokens · $0.0018 · session 019a…]
```

---

## `zcode repl` — the interactive TUI

```sh
zcode repl [OPTIONS]
```

| Flag | Value | What it does |
|------|-------|--------------|
| `--mode` | `planning` \| `editing` \| `auto` | Starting mode; change it later with `/mode` |
| `--provider` | name or kind | Starting provider; change it later with `/provider` |
| `--session` | session id | Resume an existing session |
| `--config` | file path | Use this config file |

Running `zcode` with no subcommand does the same thing.

---

## `zcode session` — session lifecycle

Sessions are JSON files under `<working_dir>/.zcode/sessions/`. See
[chapter 6](06-sessions.md).

| Command | What it does |
|---------|--------------|
| `zcode session create` | Allocate a new session id and print it |
| `zcode session continue <ID> [PROMPT]` | With a prompt, run headlessly on that session; without one, open the TUI on it (accepts `--json` and `--json-format`) |
| `zcode session fork <ID> [--as <NEW_ID>]` | Branch a session into an independent copy |
| `zcode session import <FILE>` | Import a session JSON file under a fresh id |
| `zcode session export <ID> --to <FILE>` | Write a session transcript to a file |

`--as` accepts any name matching `[A-Za-z0-9._-]{1,64}`, so a fork can be
called `before-refactor` rather than a UUID.

```sh
id=$(zcode session create)
zcode session continue "$id" "start the refactor"
zcode session fork "$id" --as before-risky-bit
zcode session export "$id" --to transcript.json
```

---

## `--provider` — pick an endpoint for one run

```sh
zcode --provider local                       # the TUI, on a different provider
zcode run --provider gateway "explain this"  # one headless run
```

The name is either an entry in the config's `providers` array or a built-in
kind. It re-resolves the model, key variable and URL together, so switching is
one word rather than four overrides. See
[chapter 12](12-configuration-reference.md#multiple-providers).

An unknown name is refused before a request is made, and says what it would
have accepted:

```
zcode: unknown provider `nope` — configured: free, fast, local, gateway;
built in: openai, anthropic, openrouter, deepseek, ollama, vllm, openai-compatible
```

---

## `zcode config` — what is actually in effect

```sh
zcode config [--config <FILE>]
```

Prints four sections: the config **sources** in override order, the **search
paths** it looked in, the **effective configuration**, and — if anything is
wrong — a **Problems** section. It exits `1` when there are problems, so it
works as a CI preflight check:

```sh
zcode config || exit 1
```

It never prints an API key, only whether the named variable is set.

The shell lines are worth reading before a first run — they say whether the
allowlist is narrow, empty, or open:

```
shell_allowed          1 pattern(s) — unrestricted: anything the denylist
                                      permits, pipes and `&&` included
shell_denied           23 built-in + 0 from config
```

---

## `zcode tools list` and `zcode skills list`

```sh
zcode tools list     # every tool the model can call, with its first description line
zcode skills list    # every skill it can load, with the roots that were searched
```

Neither needs an API key: no provider client is constructed.

---

## TUI slash commands

Type these in the prompt and press Enter. Anything that is not a recognised
command is sent to the model, so `/usr/local/bin` and `/etc/hosts` still reach
it as ordinary text.

| Command | Aliases | What it does |
|---------|---------|--------------|
| `/help` | `/?`, `/h` | List commands and keys |
| `/exit` | `/quit`, `/q` | Leave zcode |
| `/mode [NAME]` | `/m` | Show modes, or switch to `planning` / `editing` / `auto` |
| `/cost` | `/usage`, `/tokens` | Token totals and estimated spend for this session |
| `/model` | — | Which provider and model are in use |
| `/provider [NAME]` | `/providers`, `/p` | List the configured providers, or switch to one |
| `/session` | `/sessions` | Current session id and its file path |
| `/tools` | — | Tools available in the current mode |
| `/new` | `/reset` | Start a fresh session — clears the model's context |
| `/clear` | `/cls` | Clear the screen, keep the session |
| `/stop` | `/cancel` | Cancel the turn in flight |
| `/copy [all]` | — | Copy the last answer, or the whole conversation |

An unknown `/word` is reported rather than silently sent:

```
unknown command `/exitt` — /help lists them all
```

---

## TUI keys

| Key | Action |
|-----|--------|
| `Enter` | Send |
| `Alt-Enter`, `Ctrl-J` | Newline without sending |
| `Esc` | Cancel the turn in flight; if idle, clear the prompt |
| `Ctrl-C` | Cancel if busy, otherwise quit |
| `Ctrl-D` | Quit when the prompt is empty |
| `Shift-Tab` | Cycle mode: planning → editing → auto |
| `←` `→` | Move the caret |
| `Ctrl-←` `Ctrl-→`, `Alt-←` `Alt-→` | Move by word |
| `Home` / `End`, `Ctrl-A` / `Ctrl-E` | Start / end of line |
| `Backspace` / `Delete` | Delete before / at the caret |
| `Ctrl-W` | Delete the previous word |
| `Ctrl-U` / `Ctrl-K` | Delete to start / end of line |
| `Ctrl-L` | Clear both panes |
| Drag | Select; releasing copies to the clipboard |
| `Ctrl-Y` | Copy the last answer |
| Mouse wheel | Scroll the conversation |
| `PageUp` / `PageDown` | Scroll the conversation a page |
| `Ctrl-↑` / `Ctrl-↓` | Scroll one line |
| `Ctrl-Home` / `Ctrl-End` | Jump to the oldest / newest line |

zcode asks the terminal to report mouse events so the wheel can scroll, which
stops the terminal doing its own selection — so zcode does it: drag to select,
release to copy, and it says which mechanism reached the clipboard. See
[chapter 5](05-tui.md#selecting-and-copying). **Shift-drag** still gets the
terminal's own selection.

Paste works with your terminal's normal paste key (`Cmd-V`, `Ctrl-Shift-V`).
zcode enables bracketed paste, so the entire clipboard lands in the prompt at
the caret — newlines included, nothing truncated, and no accidental send.

---

## Environment variables

Every one of these overrides the config file and is overridden by a CLI flag.

| Variable | Effect |
|----------|--------|
| `ZCODE_PROVIDER` | `openai`, `anthropic`, `openrouter`, `deepseek`, `ollama`, `vllm`, `openai-compatible` |
| `ZCODE_MODEL` | Model id |
| `ZCODE_API_KEY_ENV` | Name of the variable holding the key |
| `ZCODE_BASE_URL` | Override the provider endpoint |
| `ZCODE_WORKING_DIR` | Project root |
| `ZCODE_MODE` | `planning`, `editing`, `auto` |
| `ZCODE_TIMEOUT_MS` | HTTP timeout, covering a whole streamed generation |
| `ZCODE_MAX_TURNS` | Turn cap |
| `ZCODE_MAX_TOKENS` | Output token cap per request |
| `ZCODE_MAX_TOOL_OUTPUT_CHARS` | Truncation budget for a tool result |
| `ZCODE_MAX_RETRIES` | Retries for a 429 or a transient 5xx |
| `ZCODE_RATE_LIMIT_BACKOFF_MS` | Wait after a 429 with no `Retry-After` (default 30000) |
| `ZCODE_SHELL_ALLOWED` | Newline-separated allow patterns; empty means deny-all |
| `ZCODE_SHELL_DENIED` | Newline-separated extra deny patterns; these *add* to the built-ins |
| `ZCODE_SKILLS_DIR` | Extra skills root |
| `ZCODE_<PROVIDER>_API_KEY` | The key itself, e.g. `ZCODE_OPENROUTER_API_KEY` |
| `RUST_LOG` | Log verbosity: `error`, `warn` (default), `info`, `debug`, `trace` |

---

Next: [Event reference](15-events.md)
