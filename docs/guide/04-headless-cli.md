# 4. Headless CLI (`zcode run`)

← [Your first task](03-first-task.md) · [Index](README.md) · Next: [Interactive TUI](05-tui.md)

`zcode run` executes one task and exits — the form to use in scripts, git hooks
and CI.

```
Usage: zcode run [OPTIONS] <PROMPT>

Arguments:
  <PROMPT>  The task, in natural language

Options:
      --image <FILE>          Attach an image for vision-capable models. Repeatable
      --mode <MODE>           planning (read-only) | editing (edits files) | auto (edits and runs shell)
      --provider <NAME>       Which provider to use: a name from the `providers` array, or a built-in kind
  -m, --model <PROVIDER/MODEL>
                              Model as `<provider>/<model>`, split at the first slash
      --session <SESSION>     Resume an existing session id
      --json                  Stream one JSON object per event to stdout (JSONL)
      --json-format <FORMAT>  Event schema for `--json`: `zcode` (default) or `opencode`
      --config <FILE>         Config file to use instead of ./zcode.json or ./zcode.toml
      --timeout <SECS>        Give up after this many seconds and checkpoint the session
```

## Streaming

Tokens are printed as they arrive from the provider, not buffered until the
response completes. On a slow model you watch the answer being written.

Two output modes:

| | stdout | stderr |
|---|--------|--------|
| default | model text + tool activity | the `[N step(s) · tokens · session]` summary |
| `--json` | one JSON object per event (JSONL) | warnings only (e.g. an MCP server that would not start) |

They are mutually exclusive by design: `--json` suppresses the human layer so
the stream stays machine-parseable. See [chapter 11](11-json-and-telemetry.md).

## Useful invocations

```sh
# Straightforward edit
$ zcode run "fix the clippy warning in src/lib.rs"

# Read-only review
$ zcode run --mode planning "what would break if I made Config non-Clone?"

# Continue an earlier session
$ zcode run --session 01a03bd4-8313-7b32-9809-7d9984359dda "now update the docs"

# A different config, e.g. a cheaper model for a bulk job
$ zcode run --config ci/zcode.cheap.json "regenerate the fixture files"

# A different provider for one run — model, key variable and URL move together
$ zcode run --provider local "summarise this diff"

# Provider and model in one argument, split at the first slash
$ zcode run -m openrouter/z-ai/glm-4.6 "summarise this diff"

# No slash: a model id on the provider already selected
$ zcode run -m gpt-4o-mini "summarise this diff"

# Machine-readable in opencode's schema, for a consumer written against it
$ zcode run --json --json-format opencode "list the crates"

# Bound the wall clock
$ zcode run --timeout 120 "refactor the parser module"

# Machine-readable, piped to jq
$ zcode run --json "list the crates" | jq -r 'select(.kind=="llm_delta") | .text'
```

## Timeouts and interruption

`--timeout <SECS>` caps the whole run. On expiry the agent checkpoints the
session, writes its telemetry report, and exits — no work is silently lost.

`Ctrl-C` does the same thing. The signal sets a cancellation flag that the loop
checks between steps, so the transcript is flushed before exit rather than the
process being torn down mid-write:

```sh
$ zcode run "big refactor"
^C
interrupted — session checkpointed
$ echo $?
130
```

Resume from where it stopped with `zcode run --session <id> "continue"`.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Completed |
| `1` | Failed — bad config, provider error, or a tool refused in planning mode |
| `2` | Command-line usage error |
| `130` | Interrupted with Ctrl-C (session checkpointed) |

Scripts can rely on these:

```sh
if ! zcode run --json "$TASK" > events.jsonl; then
  echo "agent failed" >&2
  exit 1
fi
```

## Working directory

Paths are resolved relative to `working_dir` — the directory you ran `zcode` in,
unless the config overrides it. Tool output reports paths relative to it too,
so lines stay short:

```
  apply_patch: patched src/main.rs (1 hunk(s))
```

---

Next: [Interactive TUI](05-tui.md)
