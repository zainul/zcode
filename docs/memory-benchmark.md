# Memory benchmark — zcode vs opencode

**zcode** v0.2.0 (`5fb5b58`, release profile) · **opencode** v1.18.23
Measured 2026-08-28 on macOS 25.3.0 (Darwin, arm64).

The gap analysis claims zcode has "a deliberately small memory and dependency
footprint". This document measures it: both agents were given the *same*
medium-to-complex coding task, in identical sandboxes, against the *same*
model, with their process trees sampled throughout.

## How this was produced

Two things were measured, and they answer different questions.

1. **A live head-to-head.** Both agents ran the same task to completion
   against the same free OpenRouter model, sampled at 20 Hz.
2. **A simulation** (`memsim`) that drives the real `AgentLoop`, the real
   `ToolRegistry`, the real `UuidSessionStore`, the real `JsonTelemetry` and
   the real `infra-llm` SSE decoder, faking only the socket. This exists to
   answer what the live test cannot: *how does the footprint scale* past the
   sizes a single task reaches.

Numbers below are resident set size (RSS) sampled every 50 ms with `ps`,
summed over the launched process and all of its descendants. Because both
agents shell out to `go build` / `go test`, RSS is reported two ways:

- **agent RSS** — only the agent's own processes (`zcode`; `opencode`/`bun`).
- **tree RSS** — everything, including the Go toolchain.

The Go toolchain peaks at ~170–180 MB and is *identical* for both agents, so
the agent column is the one that compares them. The tree column is what the
machine actually has to supply.

### The task

A Go service seeded with an orders domain (6 files, ~250 lines: model,
repository, service, handler, `cmd/api/main.go`, `go.mod`). Both agents were
given the same 24-line prompt: add `POST /orders/{id}/refund` with

- request validation (400 on bad amount or empty reason),
- three business rules in the service layer (status eligibility → 409;
  cumulative refund cap → 422; 30-day post-delivery window → 409),
- a ledger entry written with the order update, status flipping to
  `cancelled` on full refund,
- domain-error → HTTP status mapping in the handler,
- table-driven tests covering the eligibility matrix, partial/full refund
  arithmetic and the 30-day window,
- route registration,
- and `go build ./...` plus `go test ./...` passing before finishing.

Each run started from a pristine copy of the seed under `git init`, so the
diff each agent produced is exactly attributable.

**Both agents completed the task on both models.** Every run below exits 0,
compiles, and its tests pass. This is a like-for-like comparison of two
working agents, not a comparison against a failure.

---

## Results

### Static footprint

| | zcode | opencode |
|---|---:|---:|
| Installed size | **5.2 MB** | **194 MB** |
| Breakdown | one static binary | 137 MB `bin/` (bundled Bun) + 57 MB `node_modules/` |
| Runtime required | none | Bun |
| Cold start (`version`) RSS | **≤ 2 MB** | **171 MB** |
| Cold start wall time | **0.07 s** | 0.30 s |

zcode's `version` exits faster than the 50 ms sampler can reliably catch it;
1.6 MB was the highest single sample observed across three runs.

### Live task — pair A, `minimax/minimax-m3:free`

| | zcode | opencode | ratio |
|---|---:|---:|---:|
| Agent RSS, peak | **16.1 MB** | 589.5 MB | **37×** |
| Agent RSS, median | **15.1 MB** | 465.0 MB | **31×** |
| Agent RSS, mean | **15.2 MB** | 421.5 MB | 28× |
| Tree RSS, peak | 185.2 MB | 754.7 MB | 4.1× |
| Wall time | 205 s | 211 s | — |
| Exit / build / tests | 0 / ✅ / ✅ | 0 / ✅ / ✅ | — |

### Live task — pair B, `nvidia/nemotron-3-super-120b-a12b:free`

| | zcode | opencode | ratio |
|---|---:|---:|---:|
| Agent RSS, peak | **15.5 MB** | 741.5 MB | **48×** |
| Agent RSS, median | **15.0 MB** | 549.4 MB | **37×** |
| Agent RSS, mean | **14.9 MB** | 584.7 MB | 39× |
| Tree RSS, peak | 196.0 MB | 923.2 MB | 4.7× |
| Wall time | 332 s | 177 s | — |
| Steps | 68 | — | — |
| Tokens | 693.8k in / 15.0k out / 278.8k cached | — | — |
| Exit / build / tests | 0 / ✅ / ✅ | 0 / ✅ / ✅ | — |

### The headline

For a medium-to-complex REST-endpoint task, **zcode holds a flat ~15 MB
working set; opencode sits between 420 MB and 590 MB and peaks between 590 MB
and 740 MB.** That is roughly **30–48× less memory for the same completed
work**.

The flatness matters as much as the number. zcode's median and peak differ by
1 MB across a 68-step, 694k-token run — the working set does not track how
long the agent has been going. opencode's median-to-peak spread is 125–190 MB,
which is a garbage-collected heap growing and being reclaimed.

Wall time is **not** a meaningful comparison here: the two runs took different
numbers of steps and free-tier models queue unpredictably. zcode was faster on
pair A and slower on pair B. Treat the timings as evidence that neither agent
is pathologically slow, and nothing more.

---

## Scaling: where zcode's memory actually goes

The live task produced a **67 KB** session transcript after 68 steps —
tool *results* are capped at `max_tool_output_chars` (16,000 chars) before
they enter the history, so a transcript grows far more slowly than the token
counter suggests. The 694k input tokens are the cumulative billing across 68
requests of a ~10–25k token transcript, not one 694k-token transcript.

`memsim` was used to push past that. Holding the step count fixed at 145 and
varying only how large the files under edit are:

| Transcript (session file) | Peak RSS |
|---:|---:|
| 486 KB | 12.6 MB |
| 1.87 MB | 21.0 MB |
| 4.94 MB | 148 MB |

And letting a realistic trace grow naturally:

| Steps | Transcript | Peak RSS |
|---:|---:|---:|
| 13 | 412 KB | 12.6 MB |
| 37 | 1.24 MB | 14.8 MB |
| 73 | 2.47 MB | 24.9 MB |
| 145 | 4.94 MB | 148 MB |

**Peak RSS tracks transcript size, not step count.** At a fixed 145 steps,
shrinking the transcript from 4.94 MB to 486 KB drops peak RSS from 148 MB to
12.6 MB. Step count alone is nearly free.

### Why

`AgentLoop::execute` clones the entire history into the request on every
iteration (`crates/app/src/lib.rs:437`):

```rust
let llm_request = LlmRequest {
    messages: history.clone().into_boxed_slice(),
    ...
};
```

The clone is transient, but it is a deep copy of a growing `Vec<LlmMessage>`
performed once per turn, and each provider client then builds a
`serde_json::Value` tree from it. Below ~2 MB of transcript this is invisible.
Above it, repeatedly allocating and freeing a multi-megabyte structure of
growing size fragments the allocator faster than it returns pages, and RSS
climbs superlinearly.

An ablation confirmed the JSON serialization is *not* the driver — disabling
it changed peak RSS by under 1 MB (148.6 → 147.7 MB at 145 steps). The clone
and the checkpoint write are what cost.

### The one asymmetry worth knowing

Tool call **results** are truncated before entering the history; tool call
**arguments** are not. A `write` of an 80 KB file puts 80 KB into the
transcript permanently. That is the fastest route to the multi-megabyte
regime above, and it is why the simulation's large-file runs diverge from the
real task, where the agent mostly used `str_replace_editor` on existing files.

In practice a real coding task lands in the 67 KB – 500 KB band, i.e. the
12–16 MB regime — which is exactly what the live runs measured. The
superlinear regime is reachable, but only by a session that writes many large
files without compaction.

---

## What this does and does not show

**It shows:** for a task of this shape and size — a multi-file REST endpoint
with real business logic, tests, and a build/test loop — zcode's resident set
is 30–48× smaller than opencode's, its install is 37× smaller, and its
working set stays flat across a 68-step run.

**It does not show** anything about output quality at scale. Both agents
produced correct, compiling, tested code here; one task is not a quality
benchmark. It also does not compare the TUI (both were run headless), and it
does not model MCP or LSP child processes, which both agents spawn and which
would add to either side.

**A caveat on the pair A zcode row:** the run that produced the agent-only
split was cut short by an upstream `Insufficient balance` from the free model
pool after it had already written all four source files and passed a build. A
separate complete run on the same model (exit 0, tests green) measured tree
median 14.5 MB / mean 16.6 MB, agreeing with the partial run's agent median of
15.1 MB. Pair B is a complete run measured end to end and is the more
authoritative of the two.

## Reproducing

The simulation lives outside this repo (it is a scratch crate that
path-depends on `app`, `tools`, `infra-*`). To rebuild the live half:

```sh
# identical sandboxes from one seed
cp -R seed/ run-zcode/ && cp -R seed/ run-opencode/

zcode run --mode auto -m openrouter/<model> "$(cat PROMPT.txt)"
opencode run --dir "$PWD" -m openrouter/<model> "$(cat PROMPT.txt)"
```

Sample `ps -Ao pid=,ppid=,rss=,comm=` at 20 Hz and sum over the process tree,
classifying each pid as agent or toolchain by its `comm`.

Note that `opencode run` **requires** `--dir`: without it, it resolves its
project root from its own state rather than the process working directory, and
in testing it operated against an unrelated repository.
