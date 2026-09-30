# PRD: Context Efficiency — Stop Paying for Tokens the Model Does Not Need

**Document ID:** PRD-CTX-EFF-003
**Status:** Implemented in v0.7.0 — the live evaluation (§2.3) is still to run; see `code-review.md` §4
**Author:** Technical Product Manager
**Created:** 2026-09-28
**Target Releases:** v0.7.0 (Phase 1) → v0.8.0 (Phase 2) → v0.9.0 (Phase 3) — see §11
**Depends on:** PRD-AGENT-CORE-002 (`docs/prd/based-system/prd.md`), shipped through v0.6.0
**Owner:** Engineering Team

---

## 0. Executive Summary

zcode reads code wastefully. When the agent works in a repository it reads **whole
files**, lists **unfiltered directories**, has **no search tool at all** in
`planning`/`editing` mode, keeps **every tool result in the transcript for the rest
of the session**, and re-sends that whole transcript on every step. On Anthropic
routes the transcript is also **never cached**, so each step bills the whole
conversation again at full price. Input cost therefore grows roughly with the
*square* of the step count. Long tasks become expensive, then slow, and in the end
fail against the context window.

This milestone makes zcode **context-efficient by construction**. It adds seven
capabilities, plus a measurement foundation and a set of supporting read changes,
all designed around one principle:

> **Return the address, not the content.** A tool should hand the model the
> smallest thing that lets it take the next correct step (a path, a line range, a
> signature, a diff) and make getting more a cheap, explicit follow-up.

| # | Capability (user request) | What it changes | Primary lever |
|---|---------------------------|-----------------|---------------|
| 1 | **ripgrep-powered `grep`** | Native, in-process search on ripgrep's own engine, available in *every* mode | Finding code without reading files |
| 2 | **LSP tools, upgraded** | Symbol-name addressing, diagnostics, compact output, multi-server | Semantic answers instead of file reads and build logs |
| 3 | **Automatic context compaction** | A tiered engine that keeps the live context bounded | Stops transcript growth |
| 4 | **Upfront code index / CodeGraph** | A background symbol and file graph with `outline`, `symbols`, `related` and a repo map | Answers "where/what is X" from an index |
| 5 | **Native glob and filtering** | One shared discovery filter (`node_modules`, `.git`, build output, `.gitignore`, `.zcodeignore`) | Keeps junk out of every listing and search |
| 6 | **AST patching** | `edit_symbol` (edit by symbol name, with syntax checks) and tighter `str_replace` | Edit payloads cover only the lines that change |
| 7 | **Built-in prompt caching** | Correct breakpoint layout, a byte-stable prefix, cache read/write accounting | Repeated prefix billed at ~0.1× |
| — | Foundation: **token budget ledger** | Per-tool token attribution, `/context`, evaluation harness | Lets every claim above be measured |
| — | Supporting: **read and output shaping** | `read` ranges, line numbers, head+tail truncation with spill files | Makes 1–6 pay off |

**North-star metric:** *effective input cost per completed task*, which must fall
**≥ 60 %** (median) against the v0.6.0 baseline on the evaluation corpus (§10),
with **no drop in task success rate** beyond 3 percentage points.

---

## 1. Problem Statement

### 1.1 Where the tokens go today (evidence from the v0.6.0 code)

| # | Bloat source | Where (v0.6.0) | Consequence |
|---|--------------|----------------|-------------|
| B1 | `read` returns the **entire file**. It takes no range and gives no line numbers. | `crates/tools/src/native.rs` `ReadTool` | Looking at one 20-line function in a 2,000-line file costs ~2,000 lines. With no line numbers the model cannot ask for a range, and it cannot aim LSP calls. |
| B2 | `list_dir` is **unfiltered and flat**. | `format_listing` in `native.rs` | `node_modules/`, `target/`, `.git/`, `dist/` and so on reach the prompt. Walking a tree takes one call per directory. |
| B3 | **No search tool.** Search happens only through `shell` (`grep`/`rg`), which `planning` and `editing` modes deny. | `domain::modes`, `DEFAULT_SHELL_ALLOWED` | In two of three modes the only way to find a symbol is to read files until it turns up. In `auto`, raw `grep -r` output is unbounded until the 32 K-char cap. |
| B4 | **Tool results are never retired.** A file read at step 3 is re-sent on steps 4…220, even after it was edited or read again. | `AgentLoop::execute` pushes to `history`; nothing removes anything | Old, superseded and dead content dominates the prompt. |
| B5 | **The whole history is re-sent every step.** | `messages: history.clone()` per step; `max_turns = 220` | Cumulative input grows ~quadratically (§1.2). |
| B6 | **Anthropic caching covers only system + tools.** The conversation (the part that grows) gets no breakpoint. | `build_anthropic_request`: `cache_control` on system and the last tool only | Every step re-bills the whole transcript at 1×. |
| B7 | **Cache reads and writes are summed** into one `cache_tokens`. | `anthropic_cache_tokens`, `LlmFinish.cache_tokens` | Hit rate cannot be measured, and writes (1.25×) and reads (0.1×) cannot be priced correctly. |
| B8 | **Head-only truncation** at `max_tool_output_chars` (32,000). | `truncate_tool_output` in `crates/app` | Compiler errors and test summaries sit at the *end* of long output, which is exactly what gets cut. The model then re-runs the command. |
| B9 | **Edits need a prior full read.** `str_replace` needs exact `old_str`. On multiple matches it edits the first and only mentions the count afterwards. | `StrReplaceTool::str_replace` | Every edit is preceded by a full-file read. An ambiguous match can edit the wrong place before the model learns it was ambiguous. |
| B10 | **LSP tools need `line`/`character`** (0-based), which the model can only learn by reading the file. | `lsp__*` specs in `crates/tools/src/lib.rs` | The semantic tools cost *more* tokens than reading, so models skip them. |
| B11 | **Token estimate is `words × 4`.** | `domain::tokens::estimate_tokens` | Code has few spaces per token, so estimates drift badly. Anything that must react to context size (clamping, and future compaction) is working from a wrong number. |

### 1.2 Why it compounds (illustrative, not measured)

Take a 40-step task where each step adds on average 3 K tokens (a tool call plus
its result) on top of a 6 K-token base (system prompt plus tool schemas):

- Context at step *n* ≈ 6 K + 3 K·*n*, which is **126 K** at step 40.
- Cumulative input billed ≈ Σ (6 K + 3 K·*n*) for *n* = 1…40 ≈ **2.7 M tokens**.
- With the conversation cached (read at 0.1×, each step's new tail written at
  1.25×), the *effective* billed input falls to roughly **0.4 M-equivalent**
  (about 15 %).
- If the tools also return 50 % fewer tokens per step, and compaction holds the
  context under ~60 K, the effective figure falls further and the context-window
  failure goes away.

These figures illustrate the shape of the problem. Real numbers come from the
Phase 0 baseline (§10.3), and the targets in §2.3 will be re-confirmed against it.

### 1.3 Who feels it

- **Developers on paid APIs** see cost climb with task length. Large tasks feel
  unaffordable.
- **Developers on large repos and monorepos** find the agent reading into
  `node_modules`, or running out of context halfway through a task.
- **Local-model users** (Ollama, vLLM, LM Studio) have small windows (8–32 K), so
  bloat turns into outright failure rather than cost.
- **Planning-mode users** cannot search at all, so planning is slow and shallow.

---

## 2. Goals, Non-Goals, and Success Metrics

### 2.1 Goals

| # | Goal | Rationale |
|---|------|-----------|
| G1 | **Make discovery cheap.** Finding a file, symbol or usage costs tokens in proportion to the *answer*, not to the repository. | Discovery is the largest share of an agent's reading and the easiest to shrink (B1–B3, B10). |
| G2 | **Keep the live context bounded** on any task length, without losing the information the task needs. | Removes quadratic growth and context-window failures (B4, B5). |
| G3 | **Bill the repeated prefix at cache rates** on every provider that offers caching. | This is the biggest direct cost lever, and v0.6.0 uses it only partly (B6, B7). |
| G4 | **Make edits proportional to the change.** | Edit payloads and the reads before them are a large, avoidable share of tokens (B9). |
| G5 | **Measure everything.** Every token is attributable to a source, and every claim here is checked by an evaluation run. | Without attribution, "reduced bloat" is a feeling, not a result. |
| G6 | **Keep zcode's identity.** Small memory, fast cold start, a pure domain, no async runtime, graceful degradation. | These are the product's advantages over the JS baseline (CLAUDE.md, NFR-PERF-*). |

### 2.2 Non-Goals

- Embeddings, vector search or semantic RAG (see §13).
- Replacing LSP with our own type checker. The index is *syntactic*; LSP remains
  the semantic authority.
- Provider-specific server-side context management (Anthropic compaction or
  context-editing betas) as the *primary* mechanism. zcode is provider-agnostic, so
  compaction is client-side (Decision D-6).
- A background daemon or file-watcher service. Indexing runs in-process, per run.

### 2.3 Success Metrics (measured on the evaluation corpus, §10)

**Primary (must hit):**

| ID | Metric | Target vs v0.6.0 baseline |
|----|--------|---------------------------|
| M1 | **Effective input cost per completed task** (uncached input + 1.25 × cache-write (5 m TTL) + 0.1 × cache-read; 1 h writes at 2.0 ×) | **↓ ≥ 60 %** median |
| M2 | Raw prompt tokens per completed task (before caching; shows tool and compaction gains separately from caching) | ↓ ≥ 40 % median |
| M3 | Tokens returned by discovery tools (`read`, `list_dir`, `grep`, `glob`, index and LSP queries, search via `shell`) per task | ↓ ≥ 50 % median |
| M4 | Cache hit ratio (cache-read ÷ total prompt tokens) on steps ≥ 2, Anthropic routes | ≥ 75 % |
| M5 | Cache hit ratio on steps ≥ 2, OpenAI-shaped routes that support caching | ≥ 50 % |
| M6 | Runs that hit a provider context-length error | **0** (from any non-zero baseline) |
| M7 | Peak live context as a share of the model window | ≤ 80 % in 100 % of runs, long-horizon tasks included |

**Guardrails (must not regress):**

| ID | Guardrail | Threshold |
|----|-----------|-----------|
| GR1 | Task success rate (graded, §10.2) | ≥ baseline − 3 pp |
| GR2 | Steps per completed task | ≤ baseline + 10 % |
| GR3 | Wall-clock per completed task (excluding provider latency variance) | ≤ baseline + 10 % |
| GR4 | Cold start (`zcode version`) | < 300 ms (NFR-PERF-01, unchanged). Indexing never runs on this path. |
| GR5 | Release binary size | ≤ v0.6.0 + 6 MB (§9.1) |
| GR6 | `make ci` | Green, including `check-deps` and `check-arch` |

**Leading indicators (signals, not gates):** the share of search done through
`grep` rather than `shell` (target ≥ 80 %); the share of edits made with
`edit_symbol` or `str_replace` rather than a whole-file `write` on existing files
(target ≥ 90 %); median `read` size in lines; compactions per 100 steps.

---

## 3. Personas and User Stories

| ID | As a… | I want… | So that… |
|----|-------|---------|----------|
| US-01 | Developer on a paid API | long tasks to cost what the work costs, not what the transcript weighs | I can hand the agent a 100-step refactor without watching the bill |
| US-02 | Developer in a JS/TS monorepo | the agent to never list, search or read into `node_modules`, `dist` or `.next` unless I point it there | results are about *my* code |
| US-03 | Planner (`--mode planning`) | to search the codebase by text and by symbol | planning is grounded and quick, not a series of whole-file reads |
| US-04 | Developer on a local 32 K model | long sessions to keep working instead of failing on context length | local models are usable for real tasks |
| US-05 | Developer | the agent to change one method without rewriting or re-reading the whole file | edits are fast, cheap and easy to review |
| US-06 | Developer | to see what fills my context (`/context`) and how much the cache saved (`/cost`) | I can trust and tune the agent |
| US-07 | Developer | the agent to check compile errors after an edit without running a full build | fix loops are short |
| US-08 | Operator (CI, `zcode run --json`) | per-tool token attribution and cache stats in the report | I can track cost regressions in the pipeline |
| US-09 | Contributor | to add a language to the index or edit tools with a grammar and a query file | coverage grows without redesign |
| US-10 | Security-conscious user | `.env` files kept out of discovery by default | secrets do not reach a prompt by accident |

---

## 4. Solution Overview

### 4.1 The context pipeline

Each tool in this milestone has one place in the flow an agent follows on every
task:

```
 DISCOVER ──► LOCATE ──────────► INSPECT ─────────► CHANGE ──────────► VERIFY ────────► REMEMBER ────► BILL
 glob         grep (ripgrep)     outline            edit_symbol        lsp__diagnostics compaction    prompt cache
 list_dir     symbols (index)    read offset/limit  str_replace        (shell tests)    (tiers 1-3)   (read 0.1×)
 (filtered)   lsp__* by symbol   lsp__hover         apply_patch
              related (graph)
 └──────────── shared DiscoveryFilter (§5.5) ────────────┘
 └──────────────────────────── token budget ledger (§5.0) measures every arrow ─────────────────────────────┘
```

The intended workflow, which tool descriptions and the system prompt teach, is:
**search → outline → read a range → edit by symbol → check diagnostics.** Reading a
whole file is the exception.

### 4.2 Design principles

1. **Address, not content.** Return paths, line ranges, signatures and diffs. Make
   "give me more" an explicit, cheap call.
2. **Every output is bounded and says what it left out.** No silent truncation.
   Each cap ends with an actionable footer, e.g. *"showing 20 of 143 matches in 31
   files — narrow with `glob` or `path`, or page with `offset: 20`."*
3. **Deterministic bytes.** For the same inputs, tool output, tool schemas and the
   system prompt are byte-identical, with stable ordering and nothing volatile.
   This is what makes caching work (§5.7) and what makes the evaluations
   repeatable.
4. **Filters shape discovery, never explicit access.** `node_modules` is excluded
   from searches and listings, but `read node_modules/x/index.d.ts` still works.
5. **Degrade, never block.** If the index is not ready, fall back to `grep`. If LSP
   is still indexing, say so and use the code index. If summarisation fails, keep
   the deterministic compaction tiers. A missing capability never fails a task.
6. **One authority per decision.** One filter for all discovery, one line-number
   convention (1-based) for all model-facing output, and `domain::modes::denies`
   still decides tool gating.
7. **Measure before claiming.** A feature ships with its telemetry, and a phase
   exits only on evaluation results (§11).

---

## 5. Functional Requirements

Priority: **P0**: required for the phase's exit gate. **P1**: required for the
milestone. **P2**: stretch within the milestone.
Line numbers in every model-facing input and output are **1-based** unless stated
otherwise (FR-READ-03).

### 5.0 Foundation — Token Budget Ledger (FR-BUDGET)

We cannot reduce what we cannot attribute. This ships first (Phase 0).

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-BUDGET-01 | **Per-result attribution.** Every `tool_result` telemetry event carries `tokens_est`, `chars`, `truncated`, `spilled` and `category` ∈ {`discover`, `locate`, `inspect`, `change`, `verify`, `shell`, `mcp`, `other`}. | Unit test: each native tool maps to exactly one category. The JSONL `tool_result` event contains all five fields. | P0 |
| FR-BUDGET-02 | **Calibrated token estimator.** `domain::tokens` replaces `words × 4` with a character-based estimator (chars ÷ 3.6 for code-like text, ÷ 4.2 for prose, chosen by a stdlib heuristic). A per-session **calibration ratio** is learned from provider-reported prompt totals: `ratio = reported ÷ estimated`, smoothed with an EMA and clamped to [0.5, 2.0]. | On a fixture set of 20 files and prompts with known tokenizer counts, the uncalibrated error is ≤ 25 % and the calibrated error is ≤ 10 %. `domain` stays stdlib-only. | P0 |
| FR-BUDGET-03 | **Context size is anchored on provider truth.** The live-context estimate is *the last provider-reported prompt total* (for Anthropic: `input + cache_read + cache_write`) *plus the calibrated estimate of messages added since*. It is used by `WindowTable::clamp` and by compaction (§5.3). | Unit test with a fake LLM that reports usage: the estimate equals the reported total plus the calibrated delta. | P0 |
| FR-BUDGET-04 | **Report aggregates.** `.zcode/reports/*.json` adds: tokens by tool and by category, peak live context (absolute and as % of window), cache read/write totals and hit ratio, number of compactions, and tokens reclaimed. | Schema test on the report file. Existing fields unchanged (additive only). | P0 |
| FR-BUDGET-05 | **`/context` slash command (TUI).** Shows the live context broken down as: system prompt, tool schemas, repo map, summaries, user/assistant text, tool results (top 5 by size, with the step they came from), free space. | Manual smoke test. The breakdown sums to the live estimate within ±1 %. | P1 |
| FR-BUDGET-06 | **`/cost` shows caching.** Adds cache read/write tokens, hit ratio, and the estimated saving against uncached billing. | Snapshot test of the rendered `/cost` text. | P1 |
| FR-BUDGET-07 | **Evaluation harness.** `make eval-tokens` runs the corpus (§10) headless with `--json`, writes a results file, and diffs it against a committed baseline. Live-provider runs are opt-in and `#[ignore]`-gated; they are never part of `make ci`. | Running the harness twice against a fixed replay produces identical token figures. | P0 |

### 5.1 ripgrep-Powered Search (FR-SEARCH) — *user request 1*

**Current state:** there is no search tool. `grep`/`rg` are reachable only through
`shell` (denied in `planning`/`editing`), their output is unbounded until the 32 K
cap, and `rg` may not be installed.

**Decision D-1:** embed **ripgrep's own engine as libraries** (`grep-searcher`,
`grep-regex`, `ignore`, `globset`, all by ripgrep's author and the crates `rg` is
built from) in a new `infra-search` crate, and expose it as a native `grep` tool.
This gives ripgrep's speed, `.gitignore` semantics and binary detection **with no
install step, in every mode, outside the shell allowlist**. The alternative,
shelling out to an `rg` binary with auto-install like `rtk`, was rejected as the
default: it inherits the shell gate (so it would be unavailable in `planning`), it
depends on a successful install, and it costs a process spawn per call.

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-SEARCH-01 | **Native `grep` tool**, backed by `infra-search`, registered in every mode and classified read-only (not in `execute_only_tool_names`). | `planning` advertises and dispatches `grep`. `domain::modes` tests updated. | P0 |
| FR-SEARCH-02 | **Arguments:** `pattern` (Rust regex syntax; required), `literal` (bool), `path` (file or directory; default = working dir), `glob` (include filter, repeatable), `type` (ripgrep file types, e.g. `rust`, `ts`, `py`), `case` ∈ {`smart` (default), `sensitive`, `insensitive`}, `output` ∈ {`files` (default), `content`, `count`}, `context` (0–5 lines, content mode only, default 0), `multiline` (bool), `limit`, `offset` (paging). | Schema snapshot test. Bad regex → model-visible `ToolResult.error` naming the position of the problem. | P0 |
| FR-SEARCH-03 | **Cheapest mode by default.** `output: files` returns only matching paths with per-file match counts. The tool description tells the model to narrow with `files` first and ask for `content` second. | Default call on a fixture repo returns paths and counts only. | P0 |
| FR-SEARCH-04 | **Bounded, actionable output.** Defaults: 50 files (`files`), 100 lines (`content`), at most 10 lines per file with a "(+N more in this file)" marker. Each line is clipped to 200 chars, with the clip window centred on the match (minified files). A footer gives totals and the next `offset`. | Tests: a 10,000-match fixture returns within caps, with a correct footer and `offset`. | P0 |
| FR-SEARCH-05 | **Deterministic ordering.** Results are sorted by path (byte order), then by line, whatever the parallel walk order. | Property test: 20 runs with 8 threads give byte-identical output. | P0 |
| FR-SEARCH-06 | **Uses the shared DiscoveryFilter** (§5.5), so the same files are excluded as in `glob`, `list_dir` and the index. An explicit `path` into an excluded directory searches it (principle 4). | `grep foo` skips `node_modules`. `grep foo path=node_modules/pkg` searches it. | P0 |
| FR-SEARCH-07 | **Safe on hostile input.** Binary files are skipped (NUL detection, ripgrep semantics). Files over `search.max_file_bytes` (default 2 MB) are skipped and counted in the footer. Symlinks are not followed by default. Regex size and DFA limits are set. | Tests with a binary file, a 10 MB file, a symlink loop, and a catastrophic regex: none hangs or panics. | P0 |
| FR-SEARCH-08 | **Cancellable and time-bounded.** The walk observes `CancelFlag` and a per-call timeout (default 10 s) and returns partial results marked `partial: true`. | Test: cancelling mid-walk returns within 100 ms with a partial marker. | P1 |
| FR-SEARCH-09 | **Steering away from shell search.** Tool descriptions and the `auto` system prompt prefer `grep`/`glob` over `shell grep/rg/find`. Telemetry counts shell commands whose first word is `grep|rg|ag|find|fd` (leading indicator, §2.3). | The ledger reports `shell_search_calls`. | P1 |

**Example output (`output: content`):**

```
crates/app/src/lib.rs
  398:        let system = LlmMessage::system(modes::system_prompt(req.mode));
  493:                messages: history.clone().into_boxed_slice(),
crates/domain/src/modes.rs
   14:pub fn system_prompt(mode: AgentMode) -> &'static str {
[3 matches in 2 files]
```

### 5.2 LSP Tools, Upgraded (FR-LSP) — *user request 2*

**Current state:** `infra-lsp` starts **one** server per run and exposes
`lsp__goto_definition`, `lsp__find_references`, `lsp__hover` and
`lsp__rename_symbol`. All four need 0-based `line`/`character`, which the model can
only get by reading the file (B10). Rename returns edits as *advice*. There are no
diagnostics. Numbering continues from FR-LSP-04 in PRD-AGENT-CORE-002.

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-LSP-05 | **Symbol-name addressing.** Every position-taking LSP tool accepts *either* `{path, line, column}` (1-based) *or* `{symbol, path?}`, for example `symbol: "AgentLoop::execute"`. A symbol is resolved to a position through the code index (§5.4), or `workspace/symbol` when the index lacks the language. If it matches more than one symbol, the tool returns the candidates (`kind name — path:line`), up to 10, and does not guess. | Tests: resolving by name on a fixture, ambiguous name returns a candidate list, 1-based to 0-based conversion at the wire. | P0 |
| FR-LSP-06 | **Compact location rendering.** References and definitions are grouped by file as `path:line  <trimmed source line>`, deterministically ordered, capped at 50 with a total count and paging. `hover` strips markdown fences and repeated signatures and is capped at 1,500 chars. | Snapshot tests against canned server responses. | P0 |
| FR-LSP-07 | **`lsp__diagnostics`**, a read-only tool. Input: `path?` (default: every document zcode has opened or edited this session) and `severity` ∈ {`error` (default), `warning`, `all`}. Output: `path:line:col severity code message`, capped at 50. Uses pull diagnostics (`textDocument/diagnostic`, LSP 3.17) where the server supports them, otherwise cached `publishDiagnostics` pushes after a settle window (default 1.5 s since the last push, 10 s maximum). | Integration test (`#[ignore]`, rust-analyzer): introduce a type error → exactly that error is reported. | P0 |
| FR-LSP-08 | **Edit → diagnostics in one round trip.** After a successful `edit_symbol`, `str_replace_editor` or `apply_patch` on a file served by a running server, the result appends *new* errors in the edited files (at most 10, compared with before the edit) when `lsp.diagnostics_on_edit = true` (default). | Test with a fake LSP port: the edit result contains the delta, not the full list. | P1 |
| FR-LSP-09 | **Guaranteed document sync.** Every write through a zcode tool sends `didOpen`/`didChange` with the new full text before the next LSP request. This makes FR-LSP-04 verifiable: every zcode write path is covered, with a test for each. | Test: edit then `find_references` sees the post-edit text (fake port records the notifications). | P0 |
| FR-LSP-10 | **Multi-server routing by language.** Servers are chosen by file extension and **started lazily** on first use for that language, up to `lsp.max_servers` (default 3). This replaces "first server that starts wins". Idle servers are shut down after `lsp.idle_shutdown_s` (default 600 s). | Monorepo fixture (Go + TS): each query goes to the right server, and a TS server is spawned only when a TS file is queried. | P1 |
| FR-LSP-11 | **Honest readiness.** While a server is still indexing (`$/progress` not finished), a query does not block until timeout. It answers from the code index when it can, labelled `(index — LSP still indexing: 43%)`, or returns a model-visible "not ready" error. | Test with a fake port that stays in progress. | P1 |
| FR-LSP-12 | **`lsp__rename_symbol` with `apply: true`** applies the workspace edit in-process, atomically across files (every file written or none), syncs the documents, and returns a diffstat such as `renamed 14 occurrences in 6 files`. `apply: false` (default) keeps the v0.6 advice behaviour. Still denied in `planning`. | Test: a multi-file rename is applied atomically. Injected write failure → no file changed. | P1 |

### 5.3 Automatic Context Compaction Engine (FR-CTX) — *user request 3*

**Current state:** there is none. The transcript grows until the 220-step cap or a
context-length 400 (B4, B5).

**Design.** A `ContextManager` in `crates/app` runs *between* steps in
`AgentLoop::execute`. The selection policy and rendering are pure `domain` logic.
It applies three tiers in order, stopping as soon as the context is at or below
the **target**:

| Tier | Name | Mechanism | Needs the LLM? | Lossy? |
|------|------|-----------|----------------|--------|
| 1 | **Supersession** | Replace tool results made obsolete by later events: a file read that was later re-read or edited; diagnostics followed by newer diagnostics; byte-identical repeated calls. | No | No: the newer copy is in context |
| 2 | **Elision** | Outside the protected recent window, replace large tool results with a stub: tool name, arguments, first line of the result, and a pointer to its spill file (FR-READ-07). Large *arguments* of old edit calls (e.g. a 300-line `write`) become `[wrote src/x.rs: 312 lines]`. | No | Recoverable from the spill file |
| 3 | **Summarisation** | Replace the oldest span (after the first user message, before the protected window) with one structured **session summary** message produced by the LLM. | Yes | Yes, but structured and audited |

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-CTX-01 | **Trigger.** Compaction runs when the live context (FR-BUDGET-03) reaches `context.compact_at` × window (default **0.75**). For models the `WindowTable` does not know, the trigger is `context.compact_at_tokens` (default 96,000). It is checked before every provider call. | Unit test: fake usage crosses the threshold → compaction runs before the next request, and not before. | P0 |
| FR-CTX-02 | **Hysteresis.** Compaction reduces the context to `context.compact_target` × window (default **0.45**), so compactions are rare and each prefix rewrite (a cache miss, §5.7) is paid for over many later steps. | Long-horizon evaluation: ≤ 1 compaction per 25 steps (median). | P0 |
| FR-CTX-03 | **Protected content is never altered:** the system prompt, the repo map, the **first user message** (the task), the **latest user message**, the most recent `context.keep_recent_steps` steps (default 6), and any content the user pinned (`/pin`, P2). | Property test over random transcripts: protected messages are byte-identical after compaction. | P0 |
| FR-CTX-04 | **Transcript validity.** Every `tool_call` keeps its `tool_result` and vice versa. Cuts happen only at step boundaries. Role alternation stays valid for every provider adapter (Anthropic's user/assistant alternation included). | Property test: every compacted transcript serialises through all four request builders without error, with no orphaned `tool_use_id`. | P0 |
| FR-CTX-05 | **Tier 1 (supersession)** is deterministic and runs *only at a compaction event*, never on every step, because an edit to earlier history invalidates the prompt cache from that point on. | Tests per supersession rule. Cache-stability test: no history bytes change between compactions. | P0 |
| FR-CTX-06 | **Tier 2 (elision)** stubs keep what the model needs to avoid repeating work, e.g. `[elided: read src/lib.rs lines 1-400 (step 7) — 9.8k tokens; full text: .zcode/spill/<sid>/<call>.txt]`. | Snapshot tests on stub format. The spill file exists and can be read. | P0 |
| FR-CTX-07 | **Tier 3 (summarisation)** uses a fixed template with sections **Goal**, **User constraints and preferences**, **Decisions and rationale**, **Work completed**, **Current state**, **Next steps**, **Open questions / risks**. It is capped at `context.summary_max_tokens` (default 2,000). The **Files touched** section is *generated by the engine* from the tool ledger (path, operation, step), never by the LLM, so it cannot be hallucinated. | Unit test with a fake LLM: output structure is present and the Files-touched section matches the ledger exactly. | P0 |
| FR-CTX-08 | **Summariser model.** `context.compaction_model` (default: the session's model) may name a cheaper `<provider>/<model>` (same resolution as `--model`). The call is attributed in telemetry as `compaction` rather than as a task step. | Config test. The telemetry event for the summariser call has `purpose: "compaction"`. | P1 |
| FR-CTX-09 | **Failure never ends the task.** If summarisation fails (transport error, refusal, output over budget), the engine keeps the Tier 1+2 result, emits a warning, and continues. If the context is *still* over the hard limit, it applies Tier 2 to the protected window except the latest step, and then proceeds. | Fault-injection test: an LLM error during summarisation → the task continues and the warning is emitted. | P0 |
| FR-CTX-10 | **Reactive compaction.** A provider context-length rejection (already parsed by `parse_window_from_error`) triggers `learn()` on the window table, one immediate compaction, and **one** retry of the same step. | Test: a fake 400 followed by success → exactly one retry, and the learned window persists. | P0 |
| FR-CTX-11 | **Nothing is lost on disk.** Messages replaced by compaction are appended to `.zcode/sessions/<id>.archive.jsonl` before the session checkpoint is written. `session export --full` includes the archive. The session file records `compactions: [{step, tier, tokens_before, tokens_after, archived_range}]`. `SessionFile` gets a version bump, and v0.6 files still load. | Round-trip test: export --full → import reproduces the uncompacted transcript. Old-version fixture loads. | P0 |
| FR-CTX-12 | **Manual control.** `/compact [focus]` in the TUI compacts immediately, and the optional focus text is passed to the summariser ("keep details about the auth flow"). `zcode session compact <id>` does the same headless. `--no-compact` / `context.compaction = false` disables automatic compaction; FR-CTX-10 still applies unless `--no-compact` is given. | CLI and slash-command tests. | P1 |
| FR-CTX-13 | **Visible and observable.** `UiEvent::Compacted {tier, tokens_before, tokens_after}` renders as a TUI note, e.g. `context compacted 142k → 61k (summary)`. A `context_compacted` telemetry event goes into the zcode JSONL. The `opencode` JSON format maps it only if opencode's schema defines an equivalent event; otherwise it is left out (translation, not emulation). The TUI status bar shows context fill, e.g. `ctx 62%`. | Emitter tests. Status-bar snapshot. | P1 |
| FR-CTX-14 | **Read de-duplication.** A `read` of a range whose content is unchanged (content hash) and *still present unelided* in the live context returns `[unchanged since step 7 — see the earlier result]` instead of the content. | Test: read, read again → the second result is the stub. Read after an edit → full content. | P2 |

### 5.4 Upfront Code Index / CodeGraph (FR-INDEX) — *user request 4*

**Current state:** there is none. Every question about structure costs `read`
calls.

**Design.** A new `infra-index` crate builds a **syntactic** index of the
repository in a background `std::thread` at startup (no async runtime). It walks
through the DiscoveryFilter and extracts **definitions** (with kind, qualified
name, line span and a one-line signature), **imports**, and **identifier
occurrences** (name-based references). It persists incrementally under
`.zcode/index/`. From the file→file reference graph it derives a **ranked repo
map**. The index is an accelerator and a source of addresses; LSP remains the
semantic authority.

**Decision D-4 (parser): tree-sitter** with per-language `tags.scm`-style queries.
The same parse trees serve the index (§5.4) and AST patching (§5.6), so one
dependency covers two capabilities. Grammars are compiled in behind per-language
cargo features (`lang-rust`, `lang-go`, `lang-typescript`, `lang-python`, which are
the default set and match `default_lsp_servers()`). GR5 caps the binary-size cost.
Languages without a grammar still get `grep`, `glob` and LSP, but no outline or
symbol edits (graceful degradation).

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-INDEX-01 | **Background build, never blocking.** The index starts building after config load, on its own thread, at low priority. The first model request is never delayed by indexing. Tools that need the index before it is ready fall back (see FR-INDEX-09). | Cold start and time-to-first-request unchanged (GR4). Test: a query during the build returns a labelled fallback. | P0 |
| FR-INDEX-02 | **Extracted data per file:** path, language, size, mtime, content hash; definitions `{name, qualified_name, kind (fn, method, struct, enum, trait/interface, class, type, const, module), span (start–end line), signature (first line, ≤ 200 chars), parent}`; imports (raw spec, plus the resolved path where possible); identifier occurrences (name → lines). | Golden tests per language on fixture files. | P0 |
| FR-INDEX-03 | **Incremental and persistent.** Stored under `.zcode/index/v1/`. On start, files whose `(mtime, size)` are unchanged are reused; changed files are re-parsed only if their content hash differs. The format is versioned, and a corrupt or old index is discarded and rebuilt, never crashed on. | Test: touch one file → exactly one re-parse. A corrupted index file → rebuild with a warning. | P0 |
| FR-INDEX-04 | **Fresh after zcode's own edits.** Every write through a zcode tool re-indexes that file synchronously before the tool returns. External changes are detected with an mtime `stat` check on the files touched by a query, and a cheap stat-only rescan at the start of each turn. | Test: `edit_symbol` then `outline` shows the new span. An external edit is visible on the next turn. | P0 |
| FR-INDEX-05 | **`outline` tool** (read-only). Input: `path` (file, or a directory for a one-level summary). Output: the definitions tree with signatures and line spans and **no bodies**, e.g. `impl AgentLoop  L120-980` / `  fn execute(&mut self, …) -> …  L386-819`. Capped at 300 lines. | Snapshot per language. On average ≤ 15 % of the tokens of a full read on the fixture corpus. | P0 |
| FR-INDEX-06 | **`symbols` tool** (read-only). Input: `query` (exact, prefix or subsequence fuzzy match), optional `kind` and `path` filter, `limit` (default 20). Output: `kind qualified_name — path:start-end  signature`, ranked exact > prefix > fuzzy, then by graph rank. | Tests: ranking order, kind filter, determinism. | P0 |
| FR-INDEX-07 | **`related` tool** (read-only). Input: `path` or `symbol`. Output: *imports*, *imported by*, *defined in*, and *referenced in* (name-based and approximate, with the label `≈`), each capped at 20. For precise references it points the model at `lsp__find_references`. | Fixture test on a small multi-file project. | P1 |
| FR-INDEX-08 | **Repo map.** At session start, zcode renders a ranked map of the repository: files ordered by PageRank over the reference graph, personalised towards paths and identifiers named in the first prompt, each with its top-level signatures, within `index.repo_map_tokens` (default **1,024**; `0` disables it). It is placed as a second system block after the mode prompt (§5.7 cache layout). It is **frozen for the session** (stored in the session file and reused on resume) so it never invalidates the cache mid-session. | Tests: same inputs → byte-identical map. The budget is respected. Resume reuses the stored map. | P1 |
| FR-INDEX-09 | **Graceful fallback.** When the index is not ready or has no grammar for a file's language, `outline` falls back to a regex-based signature scan (`fn|func|def|class|interface|struct|type|impl` families) labelled `(approximate)`, and `symbols` falls back to a `grep` for definition patterns. | Test: index disabled → both tools still answer, labelled. | P0 |
| FR-INDEX-10 | **CLI.** `zcode index status` (files, symbols, languages, excluded counts, age, size on disk), `zcode index rebuild`, `zcode index clear`. `--no-index` / `index.enabled = false` turns it off. | CLI tests. `zcode config` prints the index settings. | P1 |
| FR-INDEX-11 | **Bounded resources.** Files over `index.max_file_bytes` (default 1 MB) and minified or generated files (FR-FILTER-06) are skipped. Each parse has a timeout (tree-sitter progress callback / cancellation, 500 ms). Memory ≤ 25 MB RSS for a 10,000-file repository (NFR-CTX-MEM-02). | Bench on a synthetic 10 k-file repo. Pathological-file test finishes within the timeout. | P0 |

### 5.5 Native Glob and Filtering (FR-FILTER, FR-GLOB) — *user request 5*

**Design.** One `DiscoveryFilter` in `infra-search` is **the single authority** on
what discovery can see. `grep`, `glob`, `list_dir`, the index and the repo map all
go through it, so no two tools disagree about whether a file exists.

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-FILTER-01 | **Layered rules**, in order: (1) built-in default excludes (Appendix C); (2) `.gitignore`, `.ignore`, `.git/info/exclude` and the global git excludes file, honoured **even outside a git repository**; (3) `.zcodeignore` files (gitignore syntax, nested like `.gitignore`); (4) config `context.exclude` (globs, added to the others); (5) config `context.include` (globs that re-admit paths excluded by 1–4). | Fixture test per layer, including precedence and nested negation (`!keep.me`). | P0 |
| FR-FILTER-02 | **Explicit access is never filtered** (principle 4). A path the model names explicitly (`read`, `grep path=…`, `list_dir path=…`, `glob path=…`) is served even if excluded. Discovery *beneath* it still applies filters other than the one that excluded the named root. | Tests: `list_dir node_modules/react` works. `grep` inside it skips its nested `.git`. | P0 |
| FR-FILTER-03 | **Secret hygiene.** `.env` and `.env.*` (except `.env.example`, `.env.sample`, `.env.template`), `*.pem`, `*.key`, `id_rsa*` and `.npmrc` are excluded from discovery by default (Appendix C). Explicit reads still work, as above. | Test: `grep API_KEY` does not surface `.env`. `read .env` works. | P0 |
| FR-FILTER-04 | **Hidden files are included** (so `.github/` and `.eslintrc` are found) unless a rule excludes them. This deliberately differs from ripgrep's default of skipping hidden files, because configuration dotfiles matter to coding tasks. | Test: `glob "**/*.yml"` finds `.github/workflows/ci.yml`. | P0 |
| FR-FILTER-05 | **Excluded directories are visible as collapsed entries** in `list_dir` (`node_modules/  (excluded)`), so the model knows they exist without seeing inside them. | Snapshot test. | P0 |
| FR-FILTER-06 | **Content heuristics** for the index and repo map (not for `grep`): skip binary files (NUL in the first 8 KB), minified files (median line length > 300 chars, or one line > 5,000 chars), generated files (a `@generated` / `DO NOT EDIT` header in the first 5 lines), and lockfiles. | Fixture tests per heuristic. | P1 |
| FR-FILTER-07 | **`zcode ignore check <path>`** reports whether a path is excluded, and by which rule and source file. | CLI test. | P2 |
| FR-GLOB-01 | **Native `glob` tool** (read-only). Input: `pattern` (globset syntax, `**` supported; repeatable), `path` (root; default = working dir), `sort` ∈ {`path` (default), `modified` (newest first)}, `type` ∈ {`file` (default), `dir`, `any`}, `limit` (default 100), `offset`. Output: relative paths, one per line, and a totals footer. | Tests: patterns, sort orders, paging, determinism under `sort: path`. | P0 |
| FR-GLOB-02 | **`list_dir` upgrade.** It gets `depth` (default 1, max 4), rendered as a compact tree. A directory with more than `list_dir.collapse_over` entries (default 50) shows a summary (`src/generated/  (214 files)`), and output is capped at 200 lines with a footer. `str_replace_editor`'s `list_dir` command shares the implementation. | Snapshot tests at depth 1 and 3. Collapse and cap behaviour. | P0 |

### 5.6 AST Patching Tools (FR-EDIT) — *user request 6*

**Current state:** `write` (whole file), `str_replace_editor` (exact
string; edits the *first* of several matches, then reports the count), `apply_patch` (unified diff
located by context). All but `apply_patch` in practice need a full read first (B9).

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-EDIT-01 | **`edit_symbol` tool** (write-class, so added to `write_tool_names`). Input: `path`, `symbol` (qualified, e.g. `AgentLoop::execute`, `UserService.create`, `handler`), `action`, `content`, and an optional `line` hint to disambiguate. Actions: `replace` (the whole span: definition plus attached doc comments, attributes and decorators), `replace_body` (only the body block; signature kept), `insert_before`, `insert_after`, `delete`. The symbol is located through tree-sitter (§5.4 D-4), not text matching. | Golden tests per action per default language. | P1 |
| FR-EDIT-02 | **Payload covers only the change.** The model sends only the new code. The response is a short confirmation with the resulting span (`replaced fn AgentLoop::execute  L386-819 → L386-801`) plus ±2 lines of context at each seam. The file content is never echoed. | Snapshot test: response ≤ 12 lines for any edit. | P1 |
| FR-EDIT-03 | **No guessing.** A symbol that is missing or ambiguous returns a model-visible error listing the candidates (`kind name — path:line`, up to 10). No write happens. | Tests for 0, 1 and several matches. | P1 |
| FR-EDIT-04 | **Syntax gate.** After an edit the file is re-parsed. If the number of `ERROR`/`MISSING` nodes went *up* compared with before the edit, the edit is rejected by default with the location of the first new error (`edit.syntax_check = "reject"`; also `"warn"` or `"off"`). Files that already had parse errors are compared against that baseline, so a pre-existing error does not block edits. | Tests: an unbalanced brace is rejected with its location. A file with an existing error can still be edited. | P1 |
| FR-EDIT-05 | **Indentation.** `content` is re-indented to the target's indentation (tabs or spaces as detected) when it is given at column 0. Otherwise it is used as-is. | Tests with tab-indented Go and space-indented Python. | P1 |
| FR-EDIT-06 | **`str_replace` is safe on ambiguity.** More than one match without `replace_all: true` becomes a model-visible error listing the matching line numbers, instead of editing the first and reporting the count afterwards. `expected_replacements: N` fails when the count differs. **Behaviour change from v0.6.0**, recorded in CHANGELOG. | Tests: two matches → error with line numbers. `replace_all` → all replaced. | P0 |
| FR-EDIT-07 | **Forgiving matching with disclosure.** If `old_str` is not found exactly, one normalised retry ignores trailing whitespace and treats indentation width as equivalent. If exactly one region matches, the edit is applied and the result says `(matched with whitespace normalisation)`. If none match, the error includes the **closest region** (best line-similarity score, ≤ 10 lines, with line numbers), so the model can retry without a full re-read. | Tests: indentation drift is applied with the note. No match → closest region shown. | P0 |
| FR-EDIT-08 | **Edit results never echo files.** `write` on an existing file, `str_replace`, `apply_patch` and `edit_symbol` return diffstats and seams, not content. `write` on an existing file over 200 lines adds a hint to use `edit_symbol` or `str_replace` next time. | Snapshot tests. | P0 |
| FR-EDIT-09 | **Indexes and LSP stay consistent.** Every successful edit triggers FR-INDEX-04 (re-index), FR-LSP-09 (document sync) and optionally FR-LSP-08 (diagnostics delta). | Integration test across all three. | P1 |

### 5.7 Built-in Prompt Caching (FR-CACHE) — *user request 7*

**Current state:** v0.6.0 has partial caching (cited in code as `FR-COST-01`, which
this section formalises and supersedes). Anthropic breakpoints are placed on the
system prompt and the last tool only, so **the conversation is never cached**
(B6). OpenAI-shaped routes place one breakpoint on the last message when an adapter
flag is set. Read and write tokens are summed (B7).

**Provider facts this design relies on** (Anthropic Messages API): caching is a
prefix match rendered in the order `tools → system → messages`; at most **4**
`cache_control` breakpoints per request; each breakpoint looks back at most **20
content positions** for an earlier entry; TTL is 5 min by default and 1 h
optionally; writes cost **1.25×** (5 min) or **2×** (1 h) and reads **~0.1×** of the
base input price; `usage.input_tokens` counts only the *uncached remainder*; each
model has a minimum cacheable prefix (512–4,096 tokens), below which nothing is
cached and no error is raised. OpenAI and DeepSeek cache prefixes automatically
and report cached tokens (`prompt_tokens_details.cached_tokens`,
`prompt_cache_hit_tokens`).

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-CACHE-01 | **Anthropic breakpoint layout** (≤ 4 total): **BP1** on the last tool definition; **BP2** on the last system block (mode prompt plus frozen repo map); **BP3** rolling, on the last content block of the newest message; **BP4** used only when the positions added since the previous request's BP3 exceed 15, placed at a position within 20 of it so the lookback still finds the earlier entry. | Payload golden tests: marker count ≤ 4 in every case, and BP4 placement on a long tool-heavy step. | P0 |
| FR-CACHE-02 | **Byte-stable prefix.** Across consecutive requests of one session, with no compaction, mode, model or tool-set change in between, request *N*'s payload with `cache_control` markers removed is a **byte prefix** of request *N+1*'s. That requires: tool specs in a fixed order (natives in registration order, MCP servers sorted by name, MCP tools sorted by name); deterministic JSON key order; no volatile data in the system prompt (time, counters, absolute temp paths); the repo map frozen per session; images attached at a stable position. | Golden test over a 5-step fake session for all four request builders. Regression test that fails if a timestamp is interpolated into the system prompt. | P0 |
| FR-CACHE-03 | **OpenAI-shaped routes.** Automatic prefix caching needs no markers. Send `prompt_cache_key = <session id>` where the endpoint accepts it (OpenAI), for better routing affinity. For OpenRouter routes to models that need explicit markers (Anthropic, Gemini), put `cache_control` on the system message *and* the last message. Marker use is **derived from the model family** instead of a hand-set adapter flag, with `cache.markers = "auto" \| "on" \| "off"` as an override. | Payload tests per provider and model family. | P0 |
| FR-CACHE-04 | **Separate accounting.** `LlmFinish` replaces `cache_tokens` with `cache_read_tokens` and `cache_write_tokens`. Parsed from: Anthropic `cache_read_input_tokens` / `cache_creation_input_tokens`; OpenAI `prompt_tokens_details.cached_tokens` (read); DeepSeek `prompt_cache_hit_tokens` (read); OpenRouter `usage.prompt_tokens_details`. Every mirror (`SessionFile`, telemetry, `opencode` translation → `tokens.cache.read` / `tokens.cache.write`) is updated. Old session files load with `cache_tokens` mapped to read. | Decoder tests with canned SSE for every provider. Old-session fixture test. | P0 |
| FR-CACHE-05 | **Correct pricing.** `domain::pricing` prices writes at 1.25× (5 min) or 2.0× (1 h) and reads at the model's cache-read rate (default 0.1× input where the table has no specific figure). `LlmFinish.cost_usd` from the provider still takes precedence (CLAUDE.md "Usage and cost"). | Pricing unit tests for each combination. | P0 |
| FR-CACHE-06 | **Correct context arithmetic.** For Anthropic, the prompt size is `input + cache_read + cache_write`, never `input` alone. This feeds FR-BUDGET-03, window clamping and compaction. | Test: a heavily cached response gives the right live-context estimate. | P0 |
| FR-CACHE-07 | **TTL policy.** `cache.ttl = "5m" \| "1h" \| "auto"` (default `auto`). In `auto` mode the TUI uses 1 h on BP1/BP2 (small, stable, reused across human think-time) and 5 min on the rolling BP3/BP4; headless uses 5 min everywhere. Longer-TTL breakpoints always come before shorter ones, as the API requires. | Payload tests per mode. | P1 |
| FR-CACHE-08 | **Known invalidators are recorded, not hidden.** Mode switch (system prompt and tool set change, because `tool_specs_for` must match `denies`), `/provider` or `/model` switch, and compaction each emit `cache_reset {reason}` telemetry and a quiet TUI note. The mode-switch cost is accepted on purpose: advertising tools that dispatch would then refuse breaks the gating invariant (CLAUDE.md "Modes"). | Telemetry tests per reason. | P1 |
| FR-CACHE-09 | **Cache regression guard.** An `#[ignore]` network test sends two identical requests and asserts `cache_read_tokens > 0` on the second (Anthropic, and OpenAI where a key is available). The evaluation harness fails a phase gate if the hit ratio drops more than 10 pp against the previous phase's result. | The test exists and is documented in `make eval-tokens`. | P1 |
| FR-CACHE-10 | **Local models.** For Ollama, send `keep_alive` (config `cache.ollama_keep_alive`, default `"30m"`) so the loaded model and its KV cache persist between steps. The byte-stable prefix (FR-CACHE-02) lets llama.cpp reuse the prefix. | Payload test. | P2 |

### 5.8 Supporting — Read and Output Shaping (FR-READ)

Without these, capabilities 1–6 cannot turn into savings: a `grep` hit is only
cheap if the follow-up read can be *just those lines*.

| ID | Requirement | Acceptance criteria | Pri |
|----|-------------|---------------------|-----|
| FR-READ-01 | **Ranged reads.** `read` gets `offset` (1-based start line) and `limit` (line count; default 400). `str_replace_editor view` gets the same through `view_range: [start, end]` (the Anthropic editor convention) and shares the implementation. | Tests: ranges, clamping at EOF, `offset` beyond EOF → model-visible error with the line count. | P0 |
| FR-READ-02 | **Large-file guard.** A read without a range of a file over `read.default_limit` lines returns the first `limit` lines, **plus the file outline** (FR-INDEX-05) when available, plus a footer: `[lines 1-400 of 2,318 — use offset/limit, or outline/symbols to find the part you need]`. | Snapshot test on a 2,000-line fixture. | P0 |
| FR-READ-03 | **Line-number gutter.** Read output prefixes every line with its 1-based number (`  412│`) so the model can address ranges, LSP positions and edit hints without counting. Every model-facing line number in zcode (grep, outline, symbols, LSP, edit results, diagnostics) uses the same 1-based convention. | Cross-tool test: a line number from `grep` fed into `read offset` and `lsp__hover line` points at the same line. | P0 |
| FR-READ-04 | **Long-line clipping.** Lines over 2,000 chars are clipped with `…[+N chars]`. Binary files are refused with a model-visible error that gives the size and detected type. | Tests. | P0 |
| FR-READ-05 | **Head + tail truncation.** Tool output over its budget keeps the **first 40 % and last 60 %** of the budget, with a marker `…[omitted 1,234 lines / 48,210 chars]…` in between, so compiler errors and test summaries at the end survive. This replaces head-only truncation (B8). | Test: a 100 k-char fake cargo output keeps the final `error[E…]` block. | P0 |
| FR-READ-06 | **Per-tool output budgets** replace the single `max_tool_output_chars` default. Defaults (tokens): `shell` 6 k, `read` 8 k, `grep` 3 k, `glob` 1.5 k, `list_dir` 1.5 k, LSP 2 k, MCP 6 k. They can be changed under `[context.tool_budgets]`. `max_tool_output_chars` stays as a global ceiling for back-compat. | Config tests. The ledger shows the budgets applied. | P1 |
| FR-READ-07 | **Spill files.** Output cut by FR-READ-05 is saved in full to `.zcode/spill/<session>/<call-id>.txt`, and the marker includes that path, so the model can `grep`/`read` the rest on demand. Spill directories are removed with their session, or after `context.spill_ttl_days` (default 7). | Test: the spill exists, the path is in the marker, and cleanup works. | P0 |

---

## 6. Tool Surface After This Milestone

| Tool | Status | Mode class | Default output budget | Notes |
|------|--------|------------|------------------------|-------|
| `grep` | **new** | read-only | 3 k tokens | ripgrep engine, §5.1 |
| `glob` | **new** | read-only | 1.5 k | §5.5 |
| `outline` | **new** | read-only | 2 k | index, §5.4 |
| `symbols` | **new** | read-only | 1.5 k | index, §5.4 |
| `related` | **new** | read-only | 1.5 k | index, §5.4 (P1) |
| `lsp__diagnostics` | **new** | read-only | 2 k | §5.2 |
| `edit_symbol` | **new** | write | seams only | §5.6 (P1) |
| `read` | changed | read-only | 8 k | ranges, gutter, guard |
| `list_dir` | changed | read-only | 1.5 k | filtered tree |
| `str_replace_editor` | changed | write | seams only | ambiguity-safe, fuzzy with disclosure |
| `apply_patch`, `write` | changed | write | diffstat | no echo |
| `lsp__goto_definition`, `lsp__find_references`, `lsp__hover` | changed | read-only | 2 k | symbol addressing, compact |
| `lsp__rename_symbol` | changed | write | diffstat | `apply: true` |
| `shell`, `zcode_skill`, `mcp__*` | unchanged (head+tail truncation, spill) | as today | 6 k | |

**Tool schema budget (NFR-CTX-TOK-01):** the full native tool schema set is
re-sent on every request (cached, but it still takes window space and is re-billed
on every cache miss). Its total must stay **≤ 3,500 tokens**. Descriptions are
written to teach the workflow in §4.1 in as few words as possible.

---

## 7. Configuration

New keys, layered like every other key (defaults → user → project → `ZCODE_*` →
flags). `zcode config` prints the effective values.

```toml
[context]
compaction          = true          # --no-compact
compact_at          = 0.75          # fraction of the model window
compact_target      = 0.45
compact_at_tokens   = 96000         # when the window is unknown
keep_recent_steps   = 6
summary_max_tokens  = 2000
compaction_model    = ""            # "" = session model; else "<provider>/<model>"
exclude             = []            # extra discovery excludes (globs)
include             = []            # re-admit excluded paths (globs)
spill_ttl_days      = 7

[context.tool_budgets]              # tokens; override per tool
shell = 6000
read  = 8000
grep  = 3000

[search]
max_file_bytes = 2_000_000
timeout_ms     = 10000

[index]
enabled         = true              # --no-index
repo_map_tokens = 1024              # 0 disables the repo map
max_file_bytes  = 1_000_000

[read]
default_limit = 400

[edit]
syntax_check = "reject"             # reject | warn | off

[lsp]
max_servers          = 3
idle_shutdown_s      = 600
diagnostics_on_edit  = true

[cache]
ttl               = "auto"          # 5m | 1h | auto
markers           = "auto"          # auto | on | off
ollama_keep_alive = "30m"
```

The equivalent `ZCODE_*` environment variables follow the existing naming scheme
(e.g. `ZCODE_CONTEXT_COMPACT_AT`, `ZCODE_INDEX_ENABLED`). Tests that touch them take
`env_guard()`.

---

## 8. Architecture Impact

The rules in CLAUDE.md hold without exception: `domain` stays stdlib-only; `app`
depends on `domain` + `thiserror` only; there is no async runtime; `cli` is the
composition root.

| Layer | Additions |
|-------|-----------|
| `domain` | Types: `SearchQuery`, `SearchHit`, `GlobQuery`, `SymbolDef`, `SymbolKind`, `Span`, `Outline`, `EditAction`. Ports: **`SearchPort`** (grep, glob, list), **`CodeIndexPort`** (outline, symbols, related, locate_symbol, symbol_span, syntax_error_count, repo_map, notify_changed). `LlmFinish`: `cache_read_tokens` / `cache_write_tokens`. `Session`: `compactions`, `repo_map`. New pure module `domain::context` (supersession rules, elision selection, stub rendering, protected-set computation). An improved `domain::tokens` (FR-BUDGET-02). |
| `app` | `ContextManager` (tiers, trigger, hysteresis, reactive retry), wired into `AgentLoop::execute` before each provider call. `UiEvent::Compacted`, `UiEvent::CacheReset`. Summariser prompt template. |
| `infra-search` (**new**) | `ignore`, `globset`, `grep-regex`, `grep-searcher`. Implements `SearchPort` and `DiscoveryFilter`. |
| `infra-index` (**new**) | `tree-sitter` plus feature-gated grammars. Depends on `infra-search` for walking (infra→infra is allowed; `dependency-check.sh` only forbids upward edges). Persistence with `serde`/`serde_json` or a compact versioned format (technical plan). Implements `CodeIndexPort`. |
| `infra-lsp` | Multi-server router, pull and push diagnostics, progress tracking, `apply` for rename. |
| `infra-llm` | Breakpoint layout (FR-CACHE-01/03/07), `prompt_cache_key`, split usage decoding. |
| `infra-session`, `infra-telemetry` | Mirror updates (`SessionFile` version bump, archive JSONL, new event kinds and fields). |
| `tools` | New tools `grep`, `glob`, `outline`, `symbols`, `related`, `edit_symbol`, `lsp__diagnostics`. Upgraded `read`, `list_dir`, `str_replace_editor`, `apply_patch`, `write`. Registration in `ToolRegistry::from_config`. `edit_symbol` is added to `domain::modes::write_tool_names`. |
| `cli` | Wiring, `/context`, `/compact`, the `ctx %` status bar item, `zcode index …`, `zcode session compact`, `zcode ignore check`. |
| `docs/architecture/dependency-check.sh` | Add `infra-search` and `infra-index` to `INFRA_CRATES`. |

**Threading.** The index builds on one background `std::thread` and shares a
read-mostly `Arc<RwLock<IndexSnapshot>>` with the tools. `grep` uses `ignore`'s
parallel walker (std threads, bounded by `available_parallelism`). Nothing
introduces tokio (CLAUDE.md "No async runtime").

**Panics.** The release profile is `panic = "abort"`, so parser and search paths
must not panic on hostile input. Fuzz targets for the tree-sitter and grep wrappers
are part of the technical plan.

---

## 9. Non-Functional Requirements

### 9.1 Performance and Footprint

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-CTX-PERF-01 | `grep` on a 100 k-file repository (Linux-kernel-sized fixture), warm cache | ≤ 1.5 × the time of `rg` for the same query on the same machine |
| NFR-CTX-PERF-02 | Cold index build, 10 k files, default languages | ≤ 20 s on a 2023 laptop. Incremental start with no changes ≤ 300 ms. Never on the critical path (FR-INDEX-01). |
| NFR-CTX-PERF-03 | `outline` / `symbols` query latency (index ready) | p95 ≤ 20 ms |
| NFR-CTX-PERF-04 | Compaction Tiers 1+2 on a 150 k-token transcript | ≤ 50 ms (Tier 3 is bounded by the provider call) |
| NFR-CTX-MEM-01 | Idle RSS, index disabled | Unchanged from v0.6.0 ± 1 MB |
| NFR-CTX-MEM-02 | Index resident memory, 10 k files | ≤ 25 MB (interned `Box<str>`, `u32` line numbers, like the `Timeline` discipline) |
| NFR-CTX-MEM-03 | Transcript memory is bounded by compaction | Long-horizon evaluation: peak RSS does not grow with step count after the first compaction |
| NFR-CTX-SIZE-01 | Release binary size | ≤ v0.6.0 + 6 MB with the four default grammars. Each grammar's cost is reported in the technical plan. |
| NFR-CTX-TOK-01 | Native tool schema total | ≤ 3,500 tokens (§6) |

### 9.2 Reliability and Correctness

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-CTX-REL-01 | No capability failure fails a task (principle 5) | Fault-injection suite: index corrupt, LSP crash, summariser error, spill directory not writable → the task continues with a warning |
| NFR-CTX-REL-02 | Determinism | Search, glob, outline, symbols, repo map and request payloads are byte-identical across runs with the same inputs |
| NFR-CTX-REL-03 | Hermetic tests | New tests pass under `cargo test --workspace` with no network and no language servers. Server- and network-dependent tests are `#[ignore]`d (CLAUDE.md conventions). |
| NFR-CTX-REL-04 | Edits are atomic | Multi-file operations (`apply_patch`, rename `apply`) write everything or nothing |

### 9.3 Security

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-CTX-SEC-01 | Secret hygiene | Discovery excludes secret-shaped files by default (FR-FILTER-03). Spill files and archives stay under `.zcode/`, which is already gitignored. |
| NFR-CTX-SEC-02 | No new execution surface | None of the new tools spawns processes except LSP servers, which are already configured. `grep`/`glob` do not go through the shell, so no allowlist bypass is possible. |
| NFR-CTX-SEC-03 | Mode gating intact | `edit_symbol` and `lsp__rename_symbol apply` are denied in `planning`. The canonical-name gating tests cover every new tool. |
| NFR-CTX-SEC-04 | Summaries are data | The summariser prompt treats transcript content as data. The summary is inserted as a clearly labelled message, never as system instructions. |

### 9.4 Compatibility

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-CTX-COMPAT-01 | Session files | v0.6 sessions load. New fields are optional. The version is bumped with a read-compatible mirror. |
| NFR-CTX-COMPAT-02 | JSON output | Additive changes only to the `zcode` JSONL schema. The `opencode` translation maps only fields opencode defines. |
| NFR-CTX-COMPAT-03 | Toolchain | Every new dependency builds on the pinned Rust **1.85.0**. The technical plan pins versions whose MSRV ≤ 1.85. |
| NFR-CTX-COMPAT-04 | Behaviour changes | FR-EDIT-06 (ambiguous `str_replace` errors) and the `read` default limit are listed in CHANGELOG under a *Changed* heading, with the reasons. |

---

## 10. Evaluation Methodology

### 10.1 Corpus

Twenty-four tasks across four fixture repositories pinned by commit, six task
categories per language family:

| Repo | Language | Size class |
|------|----------|------------|
| zcode (this repo, pinned) | Rust | ~25 k LOC |
| A Go HTTP service (open source, pinned) | Go | ~30 k LOC |
| A Next.js application with `node_modules` installed | TypeScript | ~40 k LOC + deps |
| A Python library (open source, pinned) | Python | ~20 k LOC |

Categories: **locate-and-explain** (read-only), **single-file bug fix**,
**multi-file refactor**, **rename across files**, **add a test that passes**,
**long-horizon** (≥ 50 steps: implement a small feature end to end).

### 10.2 Protocol

- Each task runs **3×** per configuration on two routes: one Anthropic-native
  model and one OpenAI-shaped route with caching. Model IDs are pinned in the
  harness config.
- Grading is automated where possible (tests pass, expected diff present, answer
  contains the required facts). Locate-and-explain tasks use a written rubric
  graded by an LLM judge, spot-checked by a person.
- Reported per task: success, steps, wall-clock, raw prompt tokens, cache
  read/write, effective input cost (M1), discovery tokens (M3), peak context (M7),
  compactions.
- Statistics: the median and IQR of per-task ratios against baseline. A target
  counts as met when the median meets it and GR1 holds.

### 10.3 Phases of measurement

1. **Baseline:** v0.6.0 plus the Phase 0 ledger only (no behaviour change).
2. After each phase in §11, and the full milestone at the end.
3. The targets in §2.3 are **re-confirmed after the baseline**. If the baseline
   shows a target is trivially met or impossible, it is revised *in this document*
   with the evidence attached, before Phase 1 starts.

---

## 11. Rollout Plan

Ordered by **savings per unit of effort** and by dependency (the filter feeds
search, search feeds the fallbacks, the index feeds edits).

| Phase | Release | Scope | Exit gate |
|-------|---------|-------|-----------|
| **0 — Measure** | v0.6.x | FR-BUDGET-01..04, 07 | Baseline recorded for the whole corpus. Targets re-confirmed (§10.3). |
| **1 — Quick wins** | **v0.7.0** | FR-CACHE-01..06 · FR-FILTER-01..05 · FR-GLOB-01..02 · FR-SEARCH-01..07 · FR-READ-01..05, 07 · FR-EDIT-06..08 | M4 met. M3 ↓ ≥ 30 %. M1 ↓ ≥ 40 %. GR1–GR6 hold. |
| **2 — Bounded context** | **v0.8.0** | FR-CTX-01..11, 13 · FR-LSP-05..07, 09 · FR-BUDGET-05..06 · FR-CACHE-07..09 · FR-READ-06 · FR-SEARCH-08..09 | M6, M7 met. Long-horizon success ≥ baseline. M2 ↓ ≥ 30 %. |
| **3 — Structure-aware** | **v0.9.0** | FR-INDEX-* · FR-EDIT-01..05, 09 · FR-LSP-08, 10..12 · FR-CTX-12 · FR-FILTER-06 | **All of §2.3 met.** NFR-CTX-SIZE-01 / MEM-02 met. |
| Stretch | any | FR-CTX-14 · FR-FILTER-07 · FR-CACHE-10 | — |

**Release mechanics.** Each capability has a config kill switch (§7) and is **on by
default** once its phase gate passes. Work ships on
`develop-release-context-efficiency-p<N>` branches merged by PR (CLAUDE.md
spec-driven workflow). Each phase updates CHANGELOG, CLAUDE.md (new tools, the
filter, compaction and caching invariants) and `examples/`.

---

## 12. Risks and Mitigations

| # | Risk | Likelihood / Impact | Mitigation |
|---|------|---------------------|------------|
| R1 | **Models keep reading whole files** even with better tools available (learned habit). | High / High | Tool descriptions teach the workflow. The large-file guard (FR-READ-02) makes the cheap path the default path. The ledger measures adoption (leading indicators). Descriptions are tuned per phase against the evaluation corpus. |
| R2 | **Summarisation drops a critical detail**, and the agent repeats or contradicts earlier work. | Medium / High | Tier 3 runs last and rarely (FR-CTX-02). Structured template. The Files-touched ledger is generated by the engine, not the LLM. The first and latest user messages and the recent steps are protected. Spill files and the archive keep everything recoverable. The long-horizon category is a gate. |
| R3 | **Compaction breaks the prompt cache** and cancels the savings. | High / Medium | Hysteresis (compact to 45 % at 75 %). Supersession runs only at compaction events. Resets are recorded (FR-CACHE-08) and the hit ratio is gated (FR-CACHE-09). |
| R4 | **Some providers reject or penalise edited history** (for example, validity checks on replayed reasoning or thinking blocks on newer models). | Medium / Medium | Compaction produces a clean new transcript rather than patching blocks in place. zcode does not replay provider reasoning blocks today; if it starts to, the technical plan must reconcile it with compaction. Provider-native compaction is kept as a future option (§14 Q3). |
| R5 | **Binary size and MSRV** grow with tree-sitter grammars. | Medium / Medium | Per-language cargo features, a measured size budget (GR5), and an MSRV check in the technical plan before adoption. Fallback: regex outline and text edits only (FR-INDEX-09), which still meets Phase 1–2 goals. |
| R6 | **A stale index** returns a wrong span, and `edit_symbol` edits the wrong code. | Low / High | Synchronous re-index on zcode's own writes. An mtime check on every file a query touches. `edit_symbol` **re-parses the current file content** right before editing and never trusts the stored spans alone. |
| R7 | **Several LSP servers use a lot of memory.** | Medium / Medium | Lazy start, `max_servers`, idle shutdown (FR-LSP-10). |
| R8 | **Filtering hides a file the task needs** (for example a vendored dependency the user is patching). | Medium / Medium | Explicit access always works (FR-FILTER-02). `context.include`. Collapsed entries show that excluded directories exist (FR-FILTER-05). `zcode ignore check` (FR-FILTER-07). |
| R9 | **The evaluation is noisy or expensive.** | Medium / Low | 3 runs per task, median-based targets, pinned repos and models, live runs opt-in only. Token figures are also checked against deterministic replays (FR-BUDGET-07). |

---

## 13. Out of Scope

1. **Embedding or vector search** and semantic retrieval. Revisit only if
   locate-task success stays below target after Phase 3.
2. **Provider-native server-side compaction or context editing** as the primary
   mechanism (Decision D-6). It may be adopted later as an adapter-level
   optimisation.
3. **File-system watchers or a background daemon** (no `notify` crate). Freshness
   comes from edit hooks plus mtime checks (FR-INDEX-04).
4. **Cross-repository or global indexes.** The index is per working directory.
5. **Precise, type-resolved references in the index.** That is LSP's job.
6. **Sub-agents / multi-agent context isolation.** It is a strong future lever
   (reading-heavy sub-tasks in a disposable context) but needs its own PRD.
7. **Grammars beyond the default four** in this milestone. The feature-flag
   mechanism is in scope; more languages are follow-up work.

---

## 14. Decisions and Open Questions

### 14.1 Resolved decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D-1 | Embed ripgrep's crates rather than shell out to `rg` | Works in every mode, no install, not subject to the shell gate, structured and deterministic output (§5.1). |
| D-2 | One `DiscoveryFilter` shared by every discovery path | Tools cannot disagree about what exists (principle 6). |
| D-3 | 1-based line numbers everywhere the model can see; conversion to LSP's 0-based at the wire | One convention removes a whole class of off-by-one tool calls. |
| D-4 | tree-sitter for both the index and AST patching, with grammars behind cargo features | One dependency serves two capabilities, with footprint control and graceful fallback. |
| D-5 | The repo map is frozen per session and stored in the session file | A map that changes mid-session invalidates the cache (FR-CACHE-02). |
| D-6 | Client-side, tiered compaction instead of provider-native features | Provider-agnostic behaviour (Ollama/vLLM included), and deterministic tiers before any lossy one. |
| D-7 | The engine, not the LLM, writes the Files-touched section of summaries | Facts that can be computed are not left to generation. |
| D-8 | Mode switches reset the cache instead of advertising a union tool set | Keeps the "advertised = permitted" invariant (CLAUDE.md "Modes"). The cost is recorded, not hidden. |

### 14.2 Open questions (to resolve in the technical plan)

| # | Question | Owner / by | Current lean |
|---|----------|-----------|--------------|
| Q1 | Index persistence format: `serde_json` (already a dependency, slow for 10 k files) or a small custom versioned binary format (no new dependency)? | Tech plan / before Phase 3 | Custom length-prefixed format, `serde_json` for metadata only |
| Q2 | Repo map on by default? It costs up to 1 K tokens per request (cached) and may not pay for itself on small repos. | Evaluation / Phase 3 gate | On for repos over 200 files, off below that. Decide from evaluation data. **v0.7.0 ships the lean; the deciding evaluation has not run yet (`code-review.md` §4).** |
| Q3 | Use Anthropic's server-side context management when on a native Anthropic route, as an *extra* layer? | Tech plan / after Phase 2 | Not in this milestone. Re-evaluate against R4. |
| Q4 | Exact tree-sitter crate and grammar versions compatible with Rust 1.85, and their measured size. | Tech plan / before Phase 3 | Must be measured, not assumed. **Resolved:** tree-sitter 0.26, rust 0.24, go 0.25, typescript 0.23, python 0.25; together +5.09 MB (fat LTO). |
| Q5 | Should `read` of a large file without a range return *only* the outline (more aggressive) rather than the first 400 lines plus the outline? | Evaluation / Phase 1 | First 400 lines plus outline. Revisit if R1 materialises. |
| Q6 | The gutter costs ~1–2 tokens per line. Keep it for all reads, or only for ranged reads? | Evaluation / Phase 1 | Keep it everywhere. Addressability pays for it. |

---

## 15. Backlog Seed

Task documents go under `docs/prd/context-efficiency/tasks/`, continuing the
workspace numbering:

| Task | Title | Phase | FRs |
|------|-------|-------|-----|
| task-21 | Token budget ledger, calibrated estimator, report aggregates | 0 | FR-BUDGET-01..04 |
| task-22 | Evaluation harness and corpus, baseline capture | 0 | FR-BUDGET-07, §10 |
| task-23 | Prompt caching: breakpoint layout, byte-stable prefix, split accounting, pricing | 1 | FR-CACHE-01..06 |
| task-24 | `infra-search` crate: DiscoveryFilter, default excludes, secret hygiene | 1 | FR-FILTER-01..05 |
| task-25 | `grep` and `glob` tools, `list_dir` tree | 1 | FR-SEARCH-01..07, FR-GLOB-01..02 |
| task-26 | Read shaping: ranges, gutter, large-file guard, head+tail, spill | 1 | FR-READ-01..05, 07 |
| task-27 | Edit safety: ambiguity errors, forgiving match, no-echo results | 1 | FR-EDIT-06..08 |
| task-28 | `ContextManager`: Tiers 1–2, trigger, hysteresis, invariants | 2 | FR-CTX-01..06, 09..10 |
| task-29 | Tier 3 summariser, archive, session v-bump, UI/telemetry | 2 | FR-CTX-07..08, 11, 13 |
| task-30 | LSP: symbol addressing, compact output, diagnostics, sync | 2 | FR-LSP-05..07, 09 |
| task-31 | Cache TTL policy, reset telemetry, regression guard; `/context`, `/cost` | 2 | FR-CACHE-07..09, FR-BUDGET-05..06 |
| task-32 | `infra-index` crate: tree-sitter extraction, persistence, freshness | 3 | FR-INDEX-01..04, 09..11 |
| task-33 | Index tools and repo map | 3 | FR-INDEX-05..08 |
| task-34 | `edit_symbol` with syntax gate and indentation | 3 | FR-EDIT-01..05, 09 |
| task-35 | LSP multi-server routing, readiness, rename apply, diagnostics-on-edit | 3 | FR-LSP-08, 10..12 |

---

## Appendix A — Glossary

- **Live context:** the prompt the next provider request will carry: system,
  tools, and messages.
- **Effective input cost:** uncached input + 1.25 × 5-min cache writes + 2.0 × 1-h
  cache writes + 0.1 × cache reads, in base-input-token equivalents (provider-specific
  read rates apply when known).
- **Supersession:** a tool result made obsolete by a later event on the same
  subject.
- **Spill file:** the full, untruncated output of a tool call, stored under
  `.zcode/spill/` and referenced from the truncated result.
- **Repo map:** a ranked, token-budgeted outline of the repository, injected once
  per session.
- **Breakpoint (BP):** a `cache_control` marker. The prompt prefix up to it can be
  served from the provider's cache.

## Appendix B — Traceability: User Request → Requirements

| User request | Requirements |
|--------------|--------------|
| 1. ripgrep as default grep | FR-SEARCH-01..09, D-1 |
| 2. LSP tool | FR-LSP-05..12, D-3 |
| 3. Automatic context compaction | FR-CTX-01..14, D-6, D-7 |
| 4. Upfront indexing / CodeGraph | FR-INDEX-01..11, D-4, D-5 |
| 5. Native glob and filtering | FR-FILTER-01..07, FR-GLOB-01..02, D-2 |
| 6. AST patching | FR-EDIT-01..09, D-4 |
| 7. Built-in prompt caching | FR-CACHE-01..10, D-8 |
| (enabling) measurement | FR-BUDGET-01..07, §10 |
| (enabling) read shaping | FR-READ-01..07 |

## Appendix C — Default Discovery Excludes

Directories: `.git/`, `.hg/`, `.svn/`, `node_modules/`, `bower_components/`,
`vendor/` (Go and PHP; re-admit with `context.include`), `target/`, `dist/`,
`build/`, `out/`, `.next/`, `.nuxt/`, `.svelte-kit/`, `.turbo/`, `.parcel-cache/`,
`.cache/`, `coverage/`, `.nyc_output/`, `__pycache__/`, `.pytest_cache/`,
`.mypy_cache/`, `.ruff_cache/`, `.tox/`, `.venv/`, `venv/`, `env/` (only when it
contains `pyvenv.cfg`), `.gradle/`, `.idea/`, `.vscode/` (except
`settings.json`/`extensions.json`), `.terraform/`, `.zcode/`, `.DS_Store`.

Files: `*.min.js`, `*.min.css`, `*.map`, `*.lock` and `package-lock.json`,
`pnpm-lock.yaml`, `yarn.lock`, `Cargo.lock`, `go.sum`, `poetry.lock` (excluded from
discovery; still readable explicitly), `*.pyc`, `*.class`, `*.o`, `*.so`,
`*.dylib`, `*.dll`, `*.exe`, `*.wasm`, images, video, audio, archives, fonts, and
PDFs.

Secret-shaped (FR-FILTER-03): `.env`, `.env.*` (except `.env.example`,
`.env.sample`, `.env.template`), `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`,
`id_ed25519*`, `.npmrc`, `.pypirc`, `.netrc`.

---

*End of document.*
