# Code Review — context-efficiency (v0.7.0)

**Review date:** 2026-09-30
**Branch:** `develop-release-context-efficiency`
**PRD:** `docs/prd/context-efficiency/prd.md` (PRD-CTX-EFF-003)
**Technical plan:** `docs/prd/context-efficiency/technical-plan.md`
**Tasks:** `docs/prd/context-efficiency/tasks/task-21..35`

---

## 1. Summary

The PRD planned three phases (v0.7 → v0.9). All three ship together as
**v0.7.0**, in 17 commits, one per task (tasks 30 and 35 took two each).
`make ci` is green: fmt, clippy `-D warnings`, 823 tests (14 ignored:
network-, binary- or time-heavy), build, domain purity, secrets scan.
`make check-arch` passes with the two new crates, `infra-search` and
`infra-index`.

| # | Capability (PRD §1) | Where it lives | Status |
|---|---|---|---|
| 1 | ripgrep as the default grep | `infra-search` (ripgrep's own crates, in-process), `grep` tool | Shipped |
| 2 | LSP tool | `lsp__*` addressed by symbol; `LspPool` (one server per language, lazy, LRU cap, idle shutdown); `lsp__diagnostics`; rename `apply` | Shipped |
| 3 | Automatic context compaction | `app::context::ContextManager` — supersession → elision → summary → emergency; reactive retry; `/compact` | Shipped |
| 4 | Upfront indexing / code graph | `infra-index` (tree-sitter), `outline` / `symbols` / `related`, repo map | Shipped |
| 5 | Native glob and filtering | `DiscoveryFilter` shared by `glob`, `grep`, `list_dir` and the index; `zcode ignore check` | Shipped |
| 6 | AST patching | `edit_symbol` (parse → locate → splice → re-parse gate), with seams-only results | Shipped |
| 7 | Built-in prompt caching | Anthropic 4-breakpoint layout, OpenAI-route markers, TTL policy, `prompt_cache_key`, cache split in `/cost` | Shipped |

## 2. Measured

All figures are from this machine (Apple Silicon, release builds unless
noted).

| What | Result | Requirement |
|---|---|---|
| Release binary | 11.29 MB vs v0.6.0's 5.52 MB → **+5.77 MB** (index +5.09, the rest +0.68) | NFR-CTX-SIZE-01: ≤ +6 MB ✔ |
| Fat LTO | needed for the size budget: thin LTO gave +6.45 MB | — |
| Index heap, 10k files / 80k defs | **20 MB** (30 MB before signatures stopped being stored) | NFR-CTX-MEM-02: ≤ 25 MB ✔ |
| Index cold build, 10k files | 1.1 s | ≤ 20 s ✔ |
| Index warm start, 10k files | 0.1 s | ≤ 300 ms ✔ |
| `symbols` query, 10k files | ≈ 3.5 ms | p95 ≤ 20 ms ✔ |
| This repo, `zcode index rebuild` | 104 files, 3,356 symbols, 378 ms, 28 MB peak RSS | — |
| `outline` vs a full `read` | 12.4 % of the tokens (45 files: larger workspace sources + eval fixtures) | FR-INDEX-05: < 15 % ✔ |
| Compaction tiers 1+2, 161k-token transcript | 0.6 ms | — |
| `grep` vs `rg` on the same tree | 1.04× | — |

## 3. Deliberate departures from the task documents

- **Replay is keyed by call sequence, not request hash (CE-DQ23a).** The
  engine's requests contain timestamps and session ids, so hashing them
  never matched a recording twice.
- **`tools` does not depend on `infra-index`.** Task 32 asked for a
  `tools/code-index` feature. Instead the tools reach the index through
  `domain::CodeIndexPort`, and the CLI owns the `code-index` feature and
  spawns the index. That keeps `tools` free of tree-sitter, and the tools
  test against the real index through a dev-dependency.
- **Signatures are not stored in the index.** A signature is the name's
  line, so it is read back from the file for the few definitions a query
  returns. Storing all of them took the 10k-file heap from 20 MB to 30 MB,
  over the budget.
- **Import resolution is not stored either.** It depends on which other
  files exist, so it is computed against the live path set when asked.
- **LSP defaults.** v0.6 started only the detected language's server, and
  no server when nothing was detected. Servers now start lazily per
  language, so every installed default is offered and detection only
  orders them. Two config tests that pinned the old rule were replaced.
- **`edit_symbol` is registered last** among the native tools, and only when
  the index is on. So `--no-index` drops a tool from the end of the cached
  tool list, not the middle.
- **One release.** The phases are not separate versions (see the CHANGELOG).

## 4. Not verified here — needs provider keys or tools this machine lacks

These are stated plainly so nobody reads the table above as covering them.

1. **The live token evaluation (PRD §2.3 M1–M7, GR1–GR3) has not been run.**
   The harness, corpus (24 tasks), graders and replay self-test are in
   `evals/`. `baseline.md` gives the capture commands (`make eval-tokens
   LIVE=1 LABEL=baseline-v0.6.0` at `7e4e0a0`, then `LABEL=p3-final` at the
   release). No provider key was available in this environment, so neither
   the baseline nor the final run exists. The primary metrics are
   **unmeasured**, not met.
2. **Q2 (should the repo map be on by default?) is still open.** The
   default is on (1,024 tokens, ≥ 200 indexed files). Settling it needs the
   with/without comparison (`repo_map_tokens = 0`) from item 1.
3. **Live prompt-cache hit rates** are covered only by `#[ignore]`d tests
   (`infra-llm`), which need keys.
4. **Peak RSS with three language servers (PRD R7)** was not measured: only
   `rust-analyzer` is installed here. The pool itself is tested with
   injected fake servers: lazy start, LRU eviction, idle shutdown, failure
   memo, and file-less queries not starting servers.
5. **Per-grammar size breakdown.** Only the total was measured: all four
   grammars plus tree-sitter cost 5.09 MB.

## 5. Risks carried forward

- **The repo map changes the system prompt**, and with it the cache prefix,
  between sessions (never within one). This is intended; it is frozen per
  session.
- **`related` and index-answered references are name-based.** They are
  labelled approximate and point at `lsp__find_references`.
- **Diagnostics-on-edit waits up to 3 s** per edited file when a running
  server covers it. A slow server delays edit results by that much at most.
