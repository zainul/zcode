# 11. JSON output & telemetry

← [Multimodal input](10-multimodal.md) · [Index](README.md) · Next: [Configuration reference](12-configuration-reference.md)

Every run is measured: model, input/output/cache tokens, step count, and
wall-clock time. Two ways to consume it — a live JSONL stream and a report file
written at the end.

## Live JSONL

```sh
$ zcode run --json "add a farewell function to src/main.rs"
```

One JSON object per line, nothing else on stdout:

```
{"cache_tokens":0,"execution_time_ms":7,"input_tokens":0,"kind":"loop_start","mode":"build","model":"demo-model","output_tokens":0,"session_id":"01a03bd4-b963-78e1-9b52-148ae44d2c51","steps":1}
{"cache_tokens":0,"execution_time_ms":9,"input_tokens":0,"kind":"llm_delta","model":"demo-model","output_tokens":0,"session_id":"01a03bd4-b963-78e1-9b52-148ae44d2c51","steps":1,"text":"I'll add the function."}
{"arguments":"{\"patch\": \"--- a/src/main.rs\\n+++ b/src/main.rs\\n@@ ...\"}","cache_tokens":0,...,"kind":"tool_call","steps":1,"tool":"apply_patch"}
{"cache_tokens":0,"error":null,...,"kind":"tool_result","steps":1,"tool":"apply_patch","truncated":false}
{"cache_tokens":0,...,"kind":"loop_start","mode":"build","steps":2}
{"cache_tokens":0,...,"kind":"llm_delta","steps":2,"text":"Added `farewell` "}
{"cache_tokens":2204,"execution_time_ms":21,"input_tokens":2490,"kind":"finish","mode":"build","model":"demo-model","output_tokens":110,"reason":"stop","session_id":"01a03bd4-...","steps":2,"truncated":false}
```

### Event kinds

| `kind` | Emitted | Extra fields |
|--------|---------|--------------|
| `loop_start` | Beginning of each step | `mode` |
| `llm_delta` | Each chunk of model text | `text` |
| `tool_call` | The model asked for a tool | `tool`, `arguments` |
| `tool_result` | The tool returned | `tool`, `error`, `truncated` |
| `tool_denied` | Refused by planning mode | `tool`, `reason` |
| `finish` | End of the run | `reason`, `truncated`, `mode` |

Every event also carries `model`, `session_id`, `steps`, `execution_time_ms`,
and the token counters. Token fields are populated on `finish`; they are `0` on
intermediate events.

`reason` is `stop` (the model finished), `tool_use`, or `length` (a cap was
hit). `truncated` is `true` when `max_turns` or `max_tokens` ended the run
early.

### Seeing which tools (and skills) were used

Every tool call — including `zcode_skill` — is visible in three places:

```sh
# 1. Normal output marks each call with `·` and shows a one-line result
$ zcode run "add a helper function"
Let me check the house style first.
· zcode_skill
  zcode_skill: House Rust conventions for this repository. (+4 more lines)
Following rust-style: every public fn gets a doc comment.

# 2. JSONL, for scripts
$ zcode run --json "..." | jq -c 'select(.kind=="tool_call") | {tool, arguments}'
{"tool":"zcode_skill","arguments":"{\"name\": \"rust-style\"}"}

# 3. The session transcript keeps a permanent record
$ jq -r '.messages[] | select(.tool_calls) | .tool_calls[].name' .zcode/sessions/<id>.json
zcode_skill
```

In the TUI, the middle pane lists them live.

### Working with the stream

```sh
# Just the answer text
$ zcode run --json "..." | jq -r 'select(.kind=="llm_delta") | .text' | tr -d '\n'

# Which tools were used
$ zcode run --json "..." | jq -r 'select(.kind=="tool_call") | .tool' | sort | uniq -c

# Token cost of this run
$ zcode run --json "..." | jq 'select(.kind=="finish") | {in:.input_tokens, out:.output_tokens, cached:.cache_tokens}'

# Fail the build if the agent hit a cap
$ zcode run --json "..." | jq -e 'select(.kind=="finish") | .truncated == false' > /dev/null
```

Every line is valid JSON on its own, so the stream is safe to pipe into `jq`,
`fluentd`, or a log collector.

## The report file

Written at the end of every run — with or without `--json`, and even when the
run is interrupted — to `.zcode/reports/<timestamp>-<session>.json`:

```json
{
  "version": 1,
  "session_id": "01a03bd4-8747-7fe0-9448-25610eed467f",
  "model": "demo-model",
  "input_tokens": 2490,
  "output_tokens": 110,
  "cache_tokens": 2204,
  "steps": 2,
  "execution_time_ms": 21,
  "finish_reason": "stop",
  "truncated": false
}
```

Aggregate across runs:

```sh
$ jq -s 'map(.input_tokens + .output_tokens) | add' .zcode/reports/*.json
$ jq -s 'group_by(.model) | map({model: .[0].model, runs: length,
         tokens: (map(.input_tokens + .output_tokens) | add)})' .zcode/reports/*.json
```

## Where the numbers come from

Token counts are **provider-reported** — read from the `usage` block of the
response, not estimated. OpenAI-compatible providers send that block in a
trailing chunk *after* the stop reason, and it is folded into the finish event.
Anthropic reports cache writes and reads separately and both are summed into
`cache_tokens`.

Only when a provider omits usage entirely (some Ollama builds) does `zcode` fall
back to a word-count heuristic. If your numbers look suspiciously round, that
is why.

`execution_time_ms` is wall-clock for the whole run, including provider latency.

## In CI

```sh
#!/usr/bin/env bash
set -euo pipefail

export ZCODE_OPENROUTER_API_KEY="$OPENROUTER_KEY"

zcode run --json --timeout 300 "$TASK" | tee events.jsonl

jq -e 'select(.kind=="finish") | .truncated == false' events.jsonl > /dev/null \
  || { echo "agent hit a cap — raising max_turns may help" >&2; exit 1; }

jq -r 'select(.kind=="tool_result" and .error != null) | "tool \(.tool): \(.error)"' events.jsonl
```

Remember the exit codes from [chapter 4](04-headless-cli.md): `0` success,
`1` failure, `2` usage error, `130` interrupted.

---

Next: [Configuration reference](12-configuration-reference.md)
