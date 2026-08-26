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
$ zcode repl --config ci/zcode.json          # a specific config
```

The TUI needs a real terminal. Without one it exits cleanly rather than
hanging:

```sh
$ zcode repl < /dev/null
zcode: Device not configured (os error 6)
```

## The screen

```
┌ conversation ─────────────────────────────────────────────┐
│ zcode: ready when you are.                                   │
│ you: add a farewell function to src/main.rs               │
│ zcode: I'll add the function.                                │
│ zcode: Added `farewell` to src/main.rs.                      │
│                                                           │
└───────────────────────────────────────────────────────────┘
┌ tools ────────────────────────────────────────────────────┐
│ · apply_patch                                             │
│   apply_patch: patched src/main.rs (1 hunk(s))            │
└───────────────────────────────────────────────────────────┘
┌ ready · 2 step(s) · 2490 in / 110 out tokens · session 01a…┐
│ > _                                                       │
└───────────────────────────────────────────────────────────┘
```

- **conversation** — your prompts and the model's replies, streaming live.
- **tools** — each tool call and a one-line summary of its result, so tool
  noise never buries the conversation.
- **input bar** — the title doubles as a status line: which provider and model,
  the current step while thinking, and the token totals when idle.

## Keys

| Key | Action |
|-----|--------|
| `Enter` | Send the prompt |
| `Backspace` | Delete a character |
| `Esc` | Cancel the turn in flight; if idle, quit |
| `Ctrl-C` | Quit |
| `q` | Quit — only when the input line is empty and no turn is running, so it never swallows typing |

Cancelling with `Esc` stops the current turn and checkpoints the session; the
REPL stays open and the next prompt starts clean.

## How it stays responsive

The engine is synchronous, so it runs on a dedicated worker thread and streams
events back over a channel while the main thread does nothing but render. A
long provider call never freezes the UI, and `Esc` is always live.

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
