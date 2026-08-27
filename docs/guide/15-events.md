# 15. Event reference

← [Command reference](14-commands.md) · [Index](README.md)

`zcode run --json` writes one JSON object per line to stdout, in one of two
schemas: zcode's own, or opencode's.

## Two formats

`--json-format` picks the schema:

```sh
zcode run --json "..."                          # zcode's own, the default
zcode run --json --json-format opencode "..."   # opencode's event envelopes
```

**zcode** is a flat log: one object per thing that happened, documented below.
**opencode** is opencode's own `session.next.*` bus shape, for tools already
written against it.

### The opencode format

Transcribed from opencode's `packages/schema/src/session-event.ts` and
`event.ts` — the field names and types are theirs, not an approximation. Every
event is an envelope:

```json
{ "id": "evt_000000000002",
  "type": "session.next.step.started",
  "data": { "timestamp": 1787749129711, "sessionID": "ses_01a03e26-…",
            "assistantMessageID": "msg_000000000001", "agent": "auto",
            "model": { "id": "claude-haiku-4.5", "providerID": "anthropic" } } }
```

`timestamp` is unix milliseconds and `sessionID` is on every payload.

| zcode emits | as opencode |
|-------------|-------------|
| a step begins | `session.next.step.started` |
| the model produces text | `session.next.text.started` → `…text.delta` → `…text.ended` |
| the model asks for a tool | `session.next.tool.called` |
| a tool succeeds | `session.next.tool.success` |
| a tool fails, or a mode refuses it | `session.next.tool.failed` |
| the provider throttles us | `session.next.retried` |
| the run ends | `session.next.step.ended`, then `session.idle` |
| a cap truncated the run | `session.error` before `session.idle` |

Faithful details worth knowing:

- `model` is a `{ id, providerID }` ref, not a string.
- `input` on `tool.called` is an **object**. A model that emits malformed JSON
  still produces a valid envelope — the raw text lands under `input.raw`.
- Deltas are live-only; `text.ended` carries the whole value, which is the
  boundary opencode expects consumers to persist.
- Tokens on `step.ended` use opencode's nested shape:
  `{ input, output, reasoning, cache: { read, write } }`. zcode measures one
  cache figure, so it is reported as `cache.read` and `cache.write` is `0`
  rather than inventing a split.
- `callID` correlates `tool.called` with its `tool.success` / `tool.failed`.
- Every `step.started` is closed by a `step.ended`. Intermediate steps close
  with `finish: "tool_use"` and zero tokens — zcode's providers report usage
  once for the whole run, so only the final step carries the totals. Mapping a
  whole run to a single opencode step would balance too, but would lose the
  step structure.

### What it does not emit

**This is a translation, not an emulation, and the gaps are deliberate.** zcode
has no message store and no event bus, so nothing durable, replayable, or
aggregate-sequenced is produced:

- no `durable` block on the envelope, and no sequence numbers
- no `message.*`, `session.created`, `session.updated`, or `permission.*`
- no `tool.input.started` / `.delta` / `.ended` — zcode streams tool arguments
  to its own UI but does not put them on the telemetry port, so there is
  nothing faithful to translate. `tool.called` carries the complete input.
- no per-step token or cost split (see above)
- no replay, no subscription, no server

An opencode client that *synchronises state* by diffing whole mutated entities
will not be satisfied by this. A client that *reads the stream* — a log parser,
a CI check, a progress display — will be. If you need the full bus, run
opencode; zcode is one process writing to stdout.

The report file is written either way: it records what happened, and is not a
rendering choice.

---

---

## Common fields

Every event carries these, so a consumer can filter without special-casing:

| Field | Type | Meaning |
|-------|------|---------|
| `kind` | string | Which event this is |
| `session_id` | string | UUIDv7 of the session |
| `model` | string | Model id in use |
| `steps` | number | 1-based step of the agent loop |
| `execution_time_ms` | number | Milliseconds since the run began |
| `input_tokens` | number | `0` except on `finish` — usage arrives at the end |
| `output_tokens` | number | Same |
| `cache_tokens` | number | Same |

Token counts are zero until `finish` because providers report usage *after* the
stop reason. Read totals from `finish`, or from the report file.

---

## The zcode events

### `loop_start`

One per step of the agent loop, before the provider is called.

```json
{"kind":"loop_start","mode":"auto","model":"anthropic/claude-haiku-4.5",
 "session_id":"01a03d78-8a60-7d00-853b-21ad80af5fd2","steps":1,
 "execution_time_ms":3,"input_tokens":0,"output_tokens":0,"cache_tokens":0}
```

| Extra field | Meaning |
|-------------|---------|
| `mode` | `planning`, `editing`, or `auto` |

### `llm_delta`

A fragment of generated text, in order. Concatenating every `text` in a step
reconstructs that step's message exactly.

```json
{"kind":"llm_delta","text":"I'll list the files in the current","steps":1, …}
```

| Extra field | Meaning |
|-------------|---------|
| `text` | The fragment. May be empty, may split mid-word, may split mid-UTF-8-grapheme across events |

### `llm_retry`

The provider returned a retryable status (429, 408, 5xx) or the connection
failed, and the client backed off. Emitted *after* the wait, before the
successful attempt's events.

```json
{"kind":"llm_retry","attempt":1,"max_attempts":3,"delay_ms":1000,
 "status":429,"reason":"rate limited by the provider","steps":1, …}
```

| Extra field | Meaning |
|-------------|---------|
| `attempt` | 1-based |
| `max_attempts` | `max_retries` from the config |
| `delay_ms` | How long the client waited — the provider's `Retry-After` if it sent one |
| `status` | HTTP status, or `null` for a transport failure |
| `reason` | Short human-readable cause |

A run that exhausts its retries fails; there is no `llm_retry` for the final
attempt, only an error.

### `tool_call`

The model asked for a tool. Arguments are the raw JSON string the model
produced — not re-serialised, so a malformed argument is visible as such.

```json
{"kind":"tool_call","tool":"list_dir","arguments":"{\"path\": \".\"}","steps":1, …}
```

### `tool_result`

The tool answered. `output` is the result *after* the
`max_tool_output_chars` cap, which is what actually entered the transcript —
never the raw result, which can be megabytes.

```json
{"kind":"tool_result","tool":"list_dir","error":null,"truncated":false,"duration_ms":19,"steps":1, …}
```

| Extra field | Meaning |
|-------------|---------|
| `tool` | Canonical tool name |
| `error` | `null` on success, else the message fed back to the model |
| `truncated` | Whether the result was cut to `max_tool_output_chars` |
| `duration_ms` | How long the call itself took, timed around the dispatch |
| `output` | The (capped) result, as the model received it |

### `tool_denied`

The current mode refused a tool. The run stops here; `finish` still follows.

```json
{"kind":"tool_denied","tool":"apply_patch","reason":"planning_mode","steps":2, …}
```

| Extra field | Meaning |
|-------------|---------|
| `reason` | `planning_mode` or `editing_mode` |

### `finish`

Exactly one per run, always last, on the failure path too.

```json
{"kind":"finish","reason":"stop","truncated":false,"mode":"auto",
 "input_tokens":7078,"output_tokens":154,"cache_tokens":0,"cost_usd":0.007848,
 "steps":2,"execution_time_ms":8028,"model":"anthropic/claude-haiku-4.5",
 "session_id":"01a03d78-8a60-7d00-853b-21ad80af5fd2"}
```

| Extra field | Meaning |
|-------------|---------|
| `reason` | `stop`, `tool_use`, or `length` |
| `truncated` | True if a turn or token cap ended the run |
| `mode` | The mode the run executed under |
| `cost_usd` | Estimated spend, or `null` when the model has no known rate |

`cost_usd` is an estimate from published list prices — see
[chapter 11](11-json-and-telemetry.md). It is **not a bill**; reconcile against
your provider's dashboard.

---

## Consuming the stream

```sh
# Just the answer
zcode run --json "…" | jq -r 'select(.kind=="llm_delta") | .text' | tr -d '\n'

# What did it touch?
zcode run --json "…" | jq -r 'select(.kind=="tool_call") | "\(.tool) \(.arguments)"'

# Did anything fail?
zcode run --json "…" | jq 'select(.kind=="tool_result" and .error != null)'

# Was it throttled?
zcode run --json "…" | jq 'select(.kind=="llm_retry")'

# Cost and totals
zcode run --json "…" | jq 'select(.kind=="finish")'

# The same run, in opencode's shape
zcode run --json --json-format opencode "…" | jq -c '{type, data}'
```

Two guarantees a consumer can rely on:

- **Exactly one `finish`**, always last, including when the run fails or is
  interrupted.
- **Unknown kinds may be added** in future versions. Ignore kinds you do not
  recognise rather than treating them as errors; existing kinds will not change
  meaning or drop fields.

## The report file

The same totals are written to
`<working_dir>/.zcode/reports/<timestamp>-<session>.json` on every run, whether
or not you passed `--json`:

```json
{
  "version": 1,
  "session_id": "01a03d78-8a60-7d00-853b-21ad80af5fd2",
  "model": "anthropic/claude-haiku-4.5",
  "input_tokens": 7078,
  "output_tokens": 154,
  "cache_tokens": 0,
  "steps": 2,
  "execution_time_ms": 8028,
  "finish_reason": "stop",
  "truncated": false,
  "cost_usd": 0.007848
}
```

`cost_usd` is omitted entirely when the model is unpriced, rather than written
as `0`.

---

← [Command reference](14-commands.md) · [Index](README.md)
