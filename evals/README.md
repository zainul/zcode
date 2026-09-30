# zcode-evals — token-efficiency evaluation harness

Measures what PRD-CTX-EFF-003 (`docs/prd/context-efficiency/prd.md`) promises:
tokens per completed task, cache hit ratio, context headroom, and that task
success does not regress. It drives the built `zcode` binary exactly as a
user does, so it measures the shipped product.

```sh
make eval-tokens                                  # hermetic replay self-test (runs in seconds, no keys)
make eval-tokens LIVE=1 LABEL=candidate           # live corpus run — needs provider keys, costs money
make eval-compare BASE=evals/results/a.json NEW=evals/results/b.json
cargo run -p zcode-evals -- run --label x --tasks go-fix,py-fix --reps 1   # a slice
```

## What is in here

| Path | Purpose |
|------|---------|
| `corpus.toml` | 24 tasks: 4 repositories × 6 categories (PRD §10.1), plus `selftest` |
| `repos.toml` | Where each task's working tree comes from. `zcode` is `git archive v0.6.0` of this repo; the rest are committed fixtures |
| `routes.toml` | The live models: one Anthropic-native, one OpenAI-shaped with caching |
| `fixtures/` | `go-service`, `next-app` (TypeScript, Node test runner), `py-lib` — each ships one deliberately failing test for its single-file-fix task |
| `setup/` | Per-repo setup run in each fresh tree. `next-app.sh` generates a `node_modules/` and `.next/` full of decoy matches; `zcode.sh` plants the Rust fix task's bug |
| `graders/` | One script per task, run in the post-run tree; exit 0 = pass |
| `replay/selftest/` | A recorded two-call session served through `ZCODE_LLM_REPLAY` |
| `results/` | Result files (`<label>.json`), one row per run |

## Graders are validated both ways

A grader that passes on an untouched tree measures nothing; one that cannot
pass measures nothing either. Every grader was checked to **fail** on a fresh
copy of its repository. The fixture graders (15) and `rust-fix`,
`rust-rename`, `rust-add-test` were also checked to **pass** against a
hand-written solution. `rust-refactor` and `rust-long` are validated in the
failing direction only; their passing conditions follow the patterns the
codebase already uses (a method on `LlmFinishReason`; a slash command parsed
by `command.rs` and tested with `parse("/version")`). Re-run the same checks
after editing a grader or a fixture.

## Metrics (PRD §2.3)

For each run, the harness reads the `zcode run --json` stream plus the run
report and records: steps, input/output tokens, cache read/write, **effective
input** (uncached + 1.25 × writes + 0.1 × reads), **raw prompt**, **discovery
tokens** (results in the `discover`/`locate`/`inspect` categories plus shell
searches), cache hit ratio, peak context, compactions, context-length errors
and shell-search calls. `compare` takes the per-(task, route) median over
repetitions, then the median of new/base ratios, and prints the M1–M7 table
with the guardrails. It exits 2 when a guardrail fails, including a cache hit
ratio drop of more than 10 points (FR-CACHE-09).

## Replay and record

`ZCODE_LLM_RECORD=<dir> zcode run …` records every provider call's event
stream to `<dir>/NNNN.jsonl`; `ZCODE_LLM_REPLAY=<dir>` serves them back in
call order with no network and no key. Replay is keyed by call *sequence*,
not by a request hash, so a recording survives changes to tool descriptions
and truncation budgets — the very things this harness measures (technical
plan CE-DQ23, as amended).

## Live runs cost money

A full run is 24 tasks × 2 routes × 3 repetitions = 144 agent runs, plus a
judge call for each rubric-graded run. Results are written after every run,
so an interrupted run keeps what it measured. Set the keys named in
`routes.toml` (`ZCODE_ANTHROPIC_API_KEY`, `ZCODE_OPENAI_API_KEY`) first.
