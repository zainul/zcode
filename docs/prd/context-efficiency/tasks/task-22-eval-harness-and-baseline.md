# Task 22 — Evaluation Harness, Corpus, Record/Replay, Baseline

**Related PRD sections:** §5.0 FR-BUDGET-07; §10 (whole section); §2.3; §11 Phase 0 gate
**Technical plan:** CE-DQ23, §4 (`evals` crate), §12
**Depends on:** task-21
**Phase:** 0 (v0.6.x)
**Status:** Done (v0.7.0)
**Priority:** Critical. No phase gate can be evaluated without it.

## Objective

A reproducible way to answer "did this change reduce tokens without hurting results?":

1. A `zcode-evals` binary that runs a corpus of tasks against the built `zcode`, grades each run, and writes a results file.
2. Record/replay in `infra-llm`, so the harness itself is testable deterministically, at no cost.
3. The **v0.6.0 + task-21 baseline**, committed, with the PRD §2.3 targets re-confirmed against it (PRD §10.3).

## Step-by-step

### 1. Workspace member `evals/`

`evals/Cargo.toml` (package `zcode-evals`, `publish = false`), dependencies `serde`, `serde_json`, `toml` (workspace versions). Add it to `[workspace] members`. It depends on **no** zcode crate: it drives the binary, as a user would.

```
evals/
  Cargo.toml
  corpus.toml            # task definitions
  repos.toml             # fixture repos: git url + pinned commit
  graders/               # grader scripts (sh) per task
  results/               # <label>.json outputs (baseline committed)
  src/main.rs            # CLI: run | compare | self-test
  src/{corpus,runner,grade,report}.rs
```

### 2. Corpus definition (`corpus.toml`)

```toml
[[task]]
id        = "rust-locate-01"
repo      = "zcode"
category  = "locate-and-explain"      # PRD §10.1 categories
mode      = "planning"
prompt    = "Where is the system prompt chosen for a mode, and what happens to it when a session resumes?"
grader    = "rubric"                  # rubric | command
rubric    = ["names domain::modes::system_prompt", "says history[0] is replaced on resume"]
max_turns = 40
```

24 tasks: 4 repos × 6 categories (PRD §10.1). `grader = "command"` tasks run `graders/<id>.sh` in the post-run working tree (exit 0 = pass: e.g. `cargo test -p x some_test`, `go test ./...`, `grep -q` for an expected edit). Long-horizon tasks set `max_turns = 120`.

`repos.toml` pins each fixture repo by commit. The runner clones into `evals/.work/<repo>@<sha>` (gitignored) and resets to a clean tree before every run (`git clean -fdx && git checkout -f <sha>`; the Next.js repo runs `npm ci` once and keeps `node_modules` outside the clean step, because the point is to test that it is filtered).

### 3. Runner (`src/runner.rs`)

For each (task, route, repetition):

```
zcode run --json --mode <mode> --max-turns <n> --model <route model> "<prompt>"  (cwd = fixture repo)
```

- Parse stdout JSONL. Read `.zcode/reports/*.json` for the run's session.
- Capture: success (from the grader), steps, wall-clock, input / output / cache read / cache write (cache is one field until task-23; the harness reads either shape), `ledger.by_category`, `context.peak_tokens`, compactions (0 until task-28), shell search calls (0 until task-31).
- Compute **effective input cost** (PRD Appendix A) and **discovery tokens** (sum of the categories `discover`, `locate`, `inspect`, plus `shell` calls whose command's first word is a search tool; the runner parses the `tool_call` event's arguments).
- `rubric` grading: send the final answer plus the rubric to a judge model (config `evals.judge_model`; default the route's model), and require every rubric line to be marked satisfied. 10 % of rubric results are written to `results/<label>.review.md` for human spot checks.

Routes come from `evals/routes.toml` (one Anthropic-native model, one OpenAI-shaped route with caching), each naming a `<provider>/<model>` and the env var holding the key.

### 4. Results and comparison (`src/report.rs`)

`results/<label>.json`: per-task rows plus aggregates (median and IQR of each metric per category and overall). `zcode-evals compare <base> <new>` prints the PRD §2.3 table: each metric's median ratio vs base, pass/fail against the targets, and GR1–GR3.

### 5. Record/replay (`crates/infra/llm/src/record.rs`)

- `ZCODE_LLM_RECORD=<dir>`: after `send_with_retry` succeeds, write the raw response body to `<dir>/<fnv64(request_json)>.sse` (FNV-1a over the serialised request with `cache_control` markers removed, so a marker-only change still replays).
- `ZCODE_LLM_REPLAY=<dir>`: `ReplayLlm` implements `LlmPort` and serves the recorded body through the **same** `SseDecode` path (`parse_*_events` helpers). A missing key is an error naming the hash.
- Selected in `cli::wire` when either env var is set. Documented as a testing aid in `zcode --help`'s environment section.

### 6. Make targets

```make
eval-tokens:        ## replay self-test (hermetic); LIVE=1 runs the corpus against real providers
	cargo run -q -p zcode-evals -- $(if $(LIVE),run --label $(LABEL),self-test)
eval-compare:
	cargo run -q -p zcode-evals -- compare $(BASE) $(NEW)
```

`self-test` replays a small recorded session (committed under `evals/replay/selftest/`) and asserts identical token figures on two runs (FR-BUDGET-07 acceptance). `make ci` runs **nothing** from `evals` except `cargo test -p zcode-evals` (pure parsing and aggregation tests).

### 7. Baseline

1. Check out v0.6.0 + task-21. Run `make eval-tokens LIVE=1 LABEL=baseline-v0.6.0`.
2. Commit `evals/results/baseline-v0.6.0.json`.
3. Write `docs/prd/context-efficiency/baseline.md`: the baseline table and, for each PRD §2.3 target, **keep / revise (with evidence)**. If anything is revised, edit PRD §2.3 in the same PR (PRD §10.3).

## Tests

- `corpus_parses_and_ids_are_unique`; `every_task_has_a_known_grader`.
- `effective_cost_formula` (PRD Appendix A, 5 m and 1 h weights).
- `aggregates_median_and_iqr`.
- `compare_flags_regression_beyond_guardrail`.
- `infra-llm`: `replay_serves_recorded_body_through_decoder`; `record_key_ignores_cache_markers`; `replay_missing_key_is_named_error`.
- `self-test` (replay) run twice → byte-identical results JSON.

## Test-case scenario

`make eval-tokens` (no network) → "self-test OK: 2 runs identical, 14 steps, 38,112 input tokens".
`make eval-tokens LIVE=1 LABEL=baseline-v0.6.0` → 24 tasks × 2 routes × 3 runs, results JSON written, summary table printed.

## How to verify

```sh
cargo test -p zcode-evals
cargo test -p infra-llm replay
make eval-tokens
make ci
```

**Pass criteria:** the self-test is deterministic; the baseline is committed with `baseline.md`; `make ci` stays hermetic (no network, no fixture clone).

## Success metric mapping

FR-BUDGET-07. The instrument for M1–M7, GR1–GR3 and every phase exit gate (PRD §11).
