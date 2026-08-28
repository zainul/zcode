# 5. Interactive TUI

← [Headless CLI](04-headless-cli.md) · [Index](README.md) · Next: [Sessions](06-sessions.md)

For multi-step work, the TUI keeps one session open across many turns so
context carries over.

## Launching

```sh
$ zcode                                   # no subcommand → TUI
$ zcode repl                              # the same thing, explicitly
$ zcode repl --mode planning              # start read-only
$ zcode repl --session <id>               # resume an existing session
$ zcode repl --config ci/zcode.json       # a specific config
$ zcode repl --provider local             # start on a different endpoint
$ zcode repl -m openrouter/z-ai/glm-4.6   # provider and model at once
```

Both spellings take the same flags: `zcode --mode planning` and `zcode repl
--mode planning` are one command written two ways. They belong after any
subcommand, though — `zcode --mode planning run "…"` is refused rather than run
with the mode quietly dropped.

The TUI needs a real terminal. Without one it exits cleanly rather than
hanging:

```sh
$ zcode repl < /dev/null
zcode: Device not configured (os error 6)
```

## The screen

Every screen in this chapter is a real capture, taken by driving the binary on
a pseudo-terminal (`examples/tui-screenshot.py`). They were recorded against
OpenRouter's free `poolside/laguna-s-2.1:free` route, which is why the cost
reads `$0.00` — that is a *known* zero, not an unknown one. An unpriced model
shows `n/a` instead.

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│20:20:43  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openrouter/poolside/laguna-s-2.1:free  │  0 in / 0 out  │  $0.00
```

- **conversation** — one timeline: your prompts, the model's replies, and the
  tools that served them, in the order they happened. Each block is stamped
  with the local wall-clock time. Your name is green, zcode's is cyan, errors
  are red.
- **prompt** — grows as you type, up to ten rows. The caret is real and visible,
  and the title tells you what Enter will do.
- **status bar** — state, mode, provider/model, running token totals, and the
  estimated cost so far. It adapts to the terminal width: on a narrow window the
  vendor namespace goes first, then the model name, then the cache count, then
  the token totals. State, mode, and **cost** always survive, because the cost
  is the field you cannot recompute by looking at the screen.

## Slash commands

`/help` lists everything, in the app:

```
┌hconversation (scrolled ↑8, PageDown to follow) ──────────────────────────────────────────────────┐
│16:36:10  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
│16:36:10  zcode                                                                                   │
│  commands:                                                                                       │
│    /help                            show this list                                               │
│    /exit                            quit zcode (also /quit, or Ctrl-C)                           │
│    /mode [planning|editing|auto]    show or change what the agent is allowed to do               │
│    /cost                            token usage and estimated spend for this session             │
│    /model                           provider, model, and config source                           │
│    /provider [NAME]                 list the configured providers, or switch to one              │
│    /session                         current session id and where it is stored                    │
│    /tools                           tools available in the current mode                          │
│    /new                             start a fresh session (clears the model's context)           │
│    /clear                           clear the screen, keep the session                           │
│    /stop                            cancel the turn in flight (also Esc)                         │
│    /copy [all]                      copy the last answer, or all of the conversation             │
│                                                                                                  │
│  keys:                                                                                           │
│    Enter                            send                                                         │
│    Alt-Enter                        newline without sending                                      │
│    Esc                              dismiss a selection, cancel the turn, or clear the prompt    │
│    Ctrl-C                           cancel if busy, otherwise quit                               │
│    Ctrl-A / Ctrl-E                  start / end of line                                          │
│    Ctrl-W                           delete the previous word                                     │
│    Ctrl-U / Ctrl-K                  delete to start / end of line                                │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openrouter/laguna-s-2.1:free  │  0 in / 0 out  │  $0.00  │  scrolled ↑8
```

The full table is in the [command reference](14-commands.md#tui-slash-commands).
Anything that is not a recognised command goes to the model, so
`what is in /usr/local/bin?` still works. A typo does not:

```
unknown command `/exitt` — /help lists them all
```

`/exit` (or `/quit`, `/q`, or Ctrl-C) leaves.

## Modes

`/mode` with no argument lists all three and marks the active one:

```
┌hconversation ────────────────────────────────────────────────────────────────────────────────────┐
│    /stop                            cancel the turn in flight (also Esc)                         │
│    /copy [all]                      copy the last answer, or all of the conversation             │
│                                                                                                  │
│  keys:                                                                                           │
│    Enter                            send                                                         │
│    Alt-Enter                        newline without sending                                      │
│    Esc                              dismiss a selection, cancel the turn, or clear the prompt    │
│    Ctrl-C                           cancel if busy, otherwise quit                               │
│    Ctrl-A / Ctrl-E                  start / end of line                                          │
│    Ctrl-W                           delete the previous word                                     │
│    Ctrl-U / Ctrl-K                  delete to start / end of line                                │
│    PageUp / PageDown                scroll the conversation (also the mouse wheel)               │
│    Ctrl-Up / Ctrl-Down              scroll one line                                              │
│    Drag                             select; releasing copies to the clipboard                    │
│    Shift-Tab                        cycle mode                                                   │
│    Ctrl-Y                           copy the last answer to the clipboard                        │
│    Up / Down                        recall the previous / next sent prompt                       │
│                                                                                                  │
│    drag to select and copy · wheel scrolls · Shift-drag for the terminal's own selection         │
│                                                                                                  │
│16:36:13  zcode                                                                                   │
│  mode: auto — edits files and runs shell                                                         │
│      planning  read-only; proposes changes                                                       │
│      editing   edits files; no shell                                                             │
│    ▸ auto      edits files and runs shell                                                        │
│    (/mode <name>, or Shift-Tab to cycle)                                                         │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openrouter/poolside/laguna-s-2.1:free  │  0 in / 0 out  │  $0.00
```

`Shift-Tab` cycles through them. The status bar follows immediately, and the
tool set changes on the next turn. See [chapter 8](08-agent-modes.md).

## Editing the prompt

The prompt is a real editor, not a single line of appended characters.

| Key | Action |
|-----|--------|
| `←` `→`, `Home` `End`, `Ctrl-A` `Ctrl-E` | Move |
| `Ctrl-←` `Ctrl-→` (or `Alt-`) | Move by word |
| `Ctrl-W` | Delete the previous word |
| `Ctrl-U` `Ctrl-K` | Delete to start / end of line |
| `Alt-Enter`, `Ctrl-J` | Newline without sending |
| `Ctrl-L` | Clear both panes |

Two presses of `Ctrl-W` on `cargo test --workspace --all-features`:

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│20:20:59  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│> cargo test                                                                                      │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openrouter/poolside/laguna-s-2.1:free  │  0 in / 0 out  │  $0.00
```

## Pasting

Paste with your terminal's normal key (`Cmd-V`, `Ctrl-Shift-V`). zcode enables
bracketed paste, so the **entire** clipboard arrives in one piece at the caret —
newlines included, nothing truncated, and no accidental send on an embedded
newline. The box grows and long lines wrap with their indentation preserved:

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│20:20:54  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│> Here is a long paste that must arrive whole. func handler(w http.ResponseWriter, r              │
│  *http.Request) {                                                                                │
│      ctx := r.Context()                                                                          │
│      if err := svc.Do(ctx); err != nil {                                                         │
│          http.Error(w, err.Error(), 500)                                                         │
│          return                                                                                  │
│      }                                                                                           │
│  }                                                                                               │
│  The final line proves nothing was truncated: SENTINEL-END                                       │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openrouter/poolside/laguna-s-2.1:free  │  0 in / 0 out  │  $0.00
```

`SENTINEL-END` is the last line of the pasted payload — it is there, so nothing
was lost.

## Tools, inline

A tool call belongs between the sentence that announced it and the sentence
that reported its result — not in a separate pane you have to correlate by eye.
Each run of calls is labelled and bracketed, and every row carries a status
icon, the time it ran, the tool, what it acted on, and how long it took:

```
20:21:35  zcode
  I'll read main.go and list the directory for you.
  tools used
  ├ ✔ 20:21:35  ◇ read      package main                          82ms
  └ ✔ 20:21:35  ▪ list_dir  .zcode/                              823ms
```

The duration is right-aligned and always carries its unit, stepping up as the
call gets longer so the number stays readable:

| Range | Reads as |
|-------|----------|
| under a second | `82ms` |
| under a minute | `1.2s` |
| under an hour | `2m05s` |
| beyond that | `1h25m` |

### Folding a run

A run of calls collapses to a single line once every call in it has settled, so
the conversation stays readable through a long session:

```
11:14:33  zcode
  Running that for you now.
  ▸ tools used · 1 call · 58ms
```

Two things stay open without being asked for:

- **Work in flight.** While a call is still running the run is open — folding
  away the only thing on screen that is changing would be exactly the wrong
  moment to hide it.
- **A failure.** The error is the text you have to act on. A header reading
  "1 failed" would say something went wrong while hiding what.

Click the header to open it, and again to fold it:

```
  ▾ tools used
  └ ✔ 11:14:33  ❯ shell  ls -lah                                          58ms
```

`Ctrl-T` folds every run at once, or opens them all if they are already folded.

A folded header carries enough to decide whether opening it is worth doing —
the number of calls, how long they took, and how many failed. You can always
fold a failure by hand; the header then accounts for it and turns red:

```
  ▸ tools used · 3 calls · 1.2s · 1 failed
```

Dragging across a header still selects it, rather than folding — otherwise the
header would be the one line in the pane you could not copy.

Each row carries two glyphs: what was called, then how it went.

| Tool | | Status | |
|------|---|--------|---|
| `◇` | read | `◐` | running |
| `▪` | list_dir | `✔` | succeeded |
| `✎` | write, str_replace_editor | `✖` | failed |
| `±` | apply_patch | `⊘` | refused by the mode gate or the shell guard |
| `❯` | shell | | |
| `✦` | zcode_skill | | |
| `⌖` | any `lsp__*` | | |
| `⊞` | any `mcp__*` | | |

The name column is measured per frame — as wide as the widest name actually on
screen, floored so a lone `read` still reads as a column and capped so one
`mcp__some_server__some_tool` cannot push the detail off the row. A fixed width
put fourteen blank cells between `read` and what it read.

<details><summary>The status icons, as a plain list</summary>

| Icon | Meaning |
|------|---------|
| `◐` | running |
| `✔` | succeeded |
| `✖` | failed — the message is the tool's own error |
| `⊘` | refused by the mode gate or the shell denylist |

</details>

A failure or a refusal settles the same row rather than adding a second one:

```
  tools used
  └ ⊘ 20:24:02  ± apply_patch  planning mode is read-only
```

A *successful* row shows **what ran** — the command, the path — rather than the
first line of what came back. `shell  ls -lah` says what happened; `shell
total 32` makes you guess. An error is the opposite: it is the thing you have
to read, so a failure replaces the invocation and, when it has more to say than
fits, wraps below the row in full instead of ending in an ellipsis:

```
  tools used
  └ ✖ 20:24:31  ❯ shell                                            1.2s
      command blocked by the shell allowlist (`shell_allowed` in
      zcode.json/zcode.toml): cd /workspace && go build ./... 2>&1 | head
        hint: no pattern in `shell_allowed` matches `cd`; add one, e.g.
        "cd( .*)?"
```

Engine notes sit inline too, with their own markers: `↻` for a retry, `!` for a
warning, `·` for information.

## Watching a turn

While the model is working, the status bar spins, counts steps, and shows
elapsed time. The prompt title changes to say what Esc does:

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│20:21:28  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
│20:21:30  you                                                                                     │
│  Read main.go, then list this directory, then stop. Use the tools.                               │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Esc cancels ─────────────────────────────────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ⠇ working · step 1/12 · 1.5s  │  mode auto  │  laguna-s-2.1:free  │  0 in / 0 out  │  $0.00
```

When it finishes, the timeline holds the record and the status bar carries the
totals:

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
│20:21:30  you                                                                                     │
│  Read main.go, then list this directory, then stop. Use the tools.                               │
│                                                                                                  │
│20:21:35  zcode                                                                                   │
│  I'll read main.go and list the directory for you.                                               │
│  tools used                                                                                      │
│  ├ ✔ 20:21:35  read               package main                                                   │
│  └ ✔ 20:21:35  list_dir           .zcode/                                                        │
│                                                                                                  │
│20:21:41  zcode                                                                                   │
│  I've read the main.go file and listed the current directory. Here's what I found:               │
│                                                                                                  │
│  **main.go contents:**                                                                           │
│  - A simple Go program with `greet` and `farewell` functions                                     │
│  - The `main` function prints greetings for "world"                                              │
│                                                                                                  │
│  **Directory contents:**                                                                         │
│  - `.zcode/` - A hidden directory (likely configuration)                                         │
│  - `demo` - A directory or file named "demo"                                                     │
│  - `go.mod` - Go module file                                                                     │
│  - `main.go` - The main Go source file                                                           │
│  - `zcode.json` - A JSON configuration file                                                      │
│                                                                                                  │
│  I've completed the requested tasks and will stop here as instructed.                            │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openrouter/poolside/laguna-s-2.1:free  │  5261 in / 189 out  │  $0.00
```

`/cost` breaks the estimate down.

## When the provider is rate limiting you

A 429 used to look identical to a hang. Now the client backs off and says so.

The wait matters as much as the message. A provider that just refused you is
still refusing you 600ms later, and free or shared tiers meter by the minute —
so a rate limit waits a flat **30 seconds** by default before trying again,
while an ordinary transient error still retries in half a second and backs off
from there.
The provider's own `Retry-After` always wins over both. Tune it with
`rate_limit_backoff_ms`.

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│20:22:18  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
│20:22:20  you                                                                                     │
│  hello                                                                                           │
│  ↻ rate limited by the provider (429) — retrying in 1.0s (attempt 1/3)                           │
│  ↻ rate limited by the provider (429) — retrying in 1.0s (attempt 2/3)                           │
│                                                                                                  │
│20:22:22  zcode                                                                                   │
│  Recovered after 2 rate limit(s).                                                                │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  openai-compatible/gpt-4o-mini  │  120 in / 8 out  │  <$0.0001
```

The status bar turns amber, the timeline keeps the record, and the turn carries
on when the provider recovers. `max_retries` (default 3) sets the
budget.

## When something fails

A provider error is shown in the conversation *and* on the status bar in red,
where it stays until the next turn starts:

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│20:22:14  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
│20:22:16  you                                                                                     │
│  hello                                                                                           │
│  ! llm error: openrouter request failed (400 Bad Request): "acme/does-not-exist is not a valid   │
│    model ID"                                                                                     │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ✖ error: llm error: openrouter request failed (400 Bad Request): "acme/does-not-exist is not a vali
```

Warnings from MCP or LSP servers appear inline as `!` note rows. They are
deliberately not written to stderr while the TUI is up: stderr is the same
terminal, and a stray log line would paint over the interface.

## Scrolling

The **mouse wheel** scrolls, three rows a notch. `PageUp` / `PageDown` move a
page, `Ctrl-↑` / `Ctrl-↓` a line, and `Ctrl-Home` / `Ctrl-End` jump to either
end. While you are scrolled back the title says so, and new output does not
yank you to the bottom; scrolling to the end resumes following the tail.

Scrolling is computed on *wrapped* rows, not logical lines, so a long answer
scrolls by what you actually see. It also stops at the oldest line rather than
counting past it — a counter that keeps rising after the view has stopped will
swallow exactly that many scrolls on the way back down, which reads as a pane
that will not scroll at all.

The wheel works because zcode asks the terminal to report mouse events, which
means the terminal stops doing its own selection. So zcode does it instead —
see below.

## Selecting and copying

**Drag to select, release to copy.** The selection highlights as you drag,
covering whole rows in between the way a terminal's own does, and on release
the text goes to the system clipboard. zcode says which mechanism it used:

```
· 3 line(s) copied (pbcopy)
```

That line is the point. Copying to a clipboard is write-only — nothing can read
it back to check — so the alternative to reporting the mechanism is announcing
success and hoping. An earlier version did exactly that, and on a terminal that
ignores the escape sequence it meant "copied" over an unchanged clipboard.

Two mechanisms, tried in order:

1. **A local clipboard tool** — `pbcopy`, `wl-copy`, `xclip`, `xsel`,
   `clip.exe`. Where one exists it is exact: the same clipboard every other
   application uses.
2. **OSC 52** — asking the terminal to set the clipboard. The only thing that
   works over SSH, where no local tool can help, but not universal: macOS
   Terminal.app ignores it, and tmux and screen need it turned on.

Clicking into the conversation moves the caret to the cell you clicked, so the
pane you are selecting from is the one showing a cursor. Typing brings it back
to the prompt.

`Esc` dismisses a highlight. It does that *before* it cancels a turn, because
cancelling a running turn when you meant to clear a selection is an expensive
misunderstanding.

The selection is in screen coordinates — it is what you can see, after
wrapping, clipping and alignment — so scrolling or new output clears it rather
than leaving a highlight pointing at words that have moved.

You do not have to drag for the common cases: `Ctrl-Y` copies the last answer,
`/copy` does the same, and `/copy all` takes the whole conversation. And the
terminal's own selection is still there under **Shift-drag** if you prefer it.

## Switching provider mid-session

When the config declares several [providers](12-configuration-reference.md#multiple-providers),
`/provider` lists them and `/provider <name>` switches:

```
┌ conversation ────────────────────────────────────────────────────────────────────────────────────┐
│10:43:26  zcode                                                                                   │
│  Ready when you are. Type /help for commands.                                                    │
│                                                                                                  │
│10:43:28  zcode                                                                                   │
│  3 provider(s) configured:                                                                       │
│    ▸ primary                                                                                     │
│      backup                                                                                      │
│      local                                                                                       │
│    (/provider <name> to switch)                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
│                                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
┌ Enter sends · Alt-Enter newline · /help ─────────────────────────────────────────────────────────┐
│>                                                                                                 │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
 ● ready  │  mode auto  │  primary/gpt-4o-mini  │  0 in / 0 out  │  $0.00
```

Only the model client is replaced. The conversation, the session, and every
MCP and LSP child process stay exactly as they were — so you can start a task
on a cheap model, hit something hard, and finish it on a better one without
losing the context that got you there.

The new client is built *before* the old one is dropped, so a typo or a missing
key leaves the working provider in place and reports the problem instead of
stranding the session with nothing to talk to. The status bar always names the
provider actually in use.

## How it stays responsive

The engine is synchronous, so it runs on a dedicated worker thread and streams
events back over a channel while the main thread does nothing but render. A
long provider call never freezes the UI, and Esc is always live.

Events and turn results share **one** channel, so a result can never overtake
the last few tokens of its own answer.

Both panes are capped at 500 lines. A tool that dumps a huge file cannot grow
the process without bound.

## Safety in the UI

Tool output is stripped of terminal escape sequences before it is displayed, so
a file containing ANSI codes cannot repaint your screen or fake UI chrome.

## TUI or headless?

| Use the TUI when | Use `zcode run` when |
|------------------|-------------------|
| Exploring, iterating, following up | Scripting or CI |
| You want to watch and interrupt | You want JSONL or an exit code |
| Context should persist across turns | One task, one result |

---

Next: [Sessions](06-sessions.md)
