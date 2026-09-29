# Task 25 — `grep` and `glob` Tools (ripgrep Engine), `list_dir` Tree

**Related PRD sections:** §5.1 FR-SEARCH-01..07; §5.5 FR-GLOB-01..02; §6 (tool surface); §9.1 NFR-CTX-PERF-01
**Technical plan:** CE-DQ2, CE-DQ13, CE-DQ14, §7.1 (`grep.rs`, `glob.rs`), §8
**Depends on:** task-24 (DiscoveryFilter, `SearchPort` types)
**Phase:** 1 (v0.7.0)
**Status:** Todo
**Priority:** High. Makes search possible in every mode (PRD B3).

## Objective

Implement `SearchPort::grep` and `SearchPort::glob` on ripgrep's crates, expose native `grep` and `glob` tools, and rebuild `list_dir` as a filtered, depth-limited tree. All three are read-only, so they are available in `planning`, `editing` and `auto`.

## Step-by-step

### 1. `infra-search/src/grep.rs`

```rust
pub(crate) fn grep(filter: &DiscoveryFilter, q: &GrepQuery, limit_files: usize,
                   cancel: &dyn Fn() -> bool) -> Result<GrepOutcome, SearchError>
```

- Matcher: `grep_regex::RegexMatcherBuilder::new()`, then `.case_smart(q.case == Smart)`, `.case_insensitive(q.case == Insensitive)`, `.fixed_strings(q.literal)`, `.multi_line(q.multiline)`, `.size_limit(10 << 20)`, `.dfa_size_limit(10 << 20)`, `.line_terminator(Some(b'\n'))` unless multiline. A build error → `SearchError::Pattern(msg)`, which becomes a model-visible error in the tool (FR-SEARCH-02).
- Walker: `filter.walker(&q.root)` plus `.types(types)` built from `ignore::types::TypesBuilder::new().add_defaults().select(t)` for each `q.types`, plus `globs` via an override *whitelist on a separate walker override layer*. Build it with `OverrideBuilder` rooted at `q.root` containing only the query's `glob` entries (whitelist semantics are wanted here). Then `.threads(min(available_parallelism, 8))` and `.build_parallel()`.
- Per worker: one `grep_searcher::SearcherBuilder` (`line_number(true)`, `binary_detection(BinaryDetection::quit(0))`, `before_context/after_context(q.context)`, `multi_line(q.multiline)`) and a custom `Sink` that collects `GrepLine { line, text (lossy UTF-8), is_context, match_col }` for up to **10 lines per file** and keeps counting beyond that.
- Skip files whose metadata size > `q.max_file_bytes` and increment `skipped_large` (FR-SEARCH-07).
- Results go over `std::sync::mpsc::channel` into a `BTreeMap<String, GrepFileHit>` on the calling thread (deterministic, FR-SEARCH-05). Workers check `cancel()` and a shared `AtomicUsize` of files found. Once it passes `limit_files`, workers stop *collecting lines* but keep counting files and matches, up to 100 000 total matches, then quit (`WalkState::Quit`). At that point the outcome is marked `partial: true` and the footer says "100000+".

### 2. `infra-search/src/glob.rs`

`GlobSet` from `q.patterns`, matched against the root-relative `/`-separated path. Walk with `filter.walker(&q.root)`. Filter by `EntryKind`. Collect `WalkEntry` with `modified_ns` from metadata. Sorting is done by the tool (FR-GLOB-01 `sort`).

### 3. `tools/src/render.rs` (new, CE-DQ13)

```rust
pub struct Footer { pub shown: usize, pub total: usize, pub unit: &'static str,
                    pub next_offset: Option<usize>, pub hint: &'static str, pub extra: Vec<String> }
impl Footer { pub fn render(&self) -> String }  // "[showing 20 of 143 files — narrow with `glob` or `path`; next: offset 20]"
pub fn clip_line(line: &str, max: usize, centre_col: usize) -> String; // char-boundary safe, "…" both sides
pub fn gutter(n: u32, width: usize) -> String;                          // "{n:>w}│"
pub fn rel_path(root: &Path, p: &Path) -> String;                        // '/'-separated, relative
```

Every tool in this task renders through these helpers, and every later task reuses them.

### 4. `tools/src/search_tools.rs`: `GrepTool`

Spec (keep the description short; it is re-sent every request, NFR-CTX-TOK-01):

```
name: grep
description: "Search file contents with ripgrep (regex; respects .gitignore; skips node_modules, build output, binaries).
 Start with output=files (default) to find where, then output=content with a narrower path/glob to see lines.
 Line numbers are 1-based and work with read offset."
params: pattern*, literal, path, glob[], type[], case(smart|sensitive|insensitive), output(files|content|count),
        context(0-5), multiline, limit, offset
```

Rendering:

- `files`: `path  (N matches)` per line, then the footer. Default `limit` 50.
- `content`: grouped by file; header line = path; each line `{gutter}{clip_line(text, 200, match_col)}`, context lines using `-` in place of `│`; `(+N more in this file)` after 10 lines. Default `limit` 100 **lines**.
- `count`: `path: N` lines plus a total.
- Empty result: `no matches for /pattern/ in <path> (N files searched)`. That is not an error.
- Always end with the `Footer`, whose `extra` includes `N large files skipped` / `partial: cancelled` when applicable.
- `subject: Some(Subject::Search { key: fnv64(normalised args) })` (lets supersession collapse identical repeats, task-28).

`GrepTool` holds `Arc<dyn SearchPort + Send + Sync>`, the project root, and a clone of the engine's cancel flag, so `Ctrl-C` interrupts a long walk (FR-SEARCH-08 lands fully in task-31 with the timeout).

### 5. `GlobTool`

Params: `pattern` (string or array)*, `path`, `sort` (path|modified), `type` (file|dir|any), `limit` (100), `offset`. Output: relative paths, one per line, directories suffixed with `/`, then the footer. `modified` sorts newest first and then by path for ties (deterministic).

### 6. `ListDirTool` rebuild (FR-GLOB-02)

Params: `path`*, `depth` (1..=4, default 1). Uses `SearchPort::list`. Render as a tree:

```
crates/
  app/
    src/  (3 files)
  domain/
node_modules/  (excluded)
Cargo.toml
[42 entries]
```

- A directory with more than `collapse_over` (50) children at the last rendered depth is shown as `name/  (N files, M dirs)`, without its children.
- Cap: 200 lines, then the footer.
- `str_replace_editor`'s `list_dir` command calls the same function (`format_listing` is removed).
- `subject: Some(Subject::Listing { path })`.

### 7. Registration and modes

- `ToolRegistry::from_config`: build one `Arc<RipgrepSearch>` from `cfg` (filter config, `search.max_file_bytes`) and register `glob`, `grep` in the fixed order from technical plan §8. `with_search(arc)` stores it for later tasks.
- `domain::modes`: nothing to add (read-only by absence). Extend the mode tests: `grep_glob_are_allowed_in_every_mode`.
- System prompts (`domain/src/modes.rs`): the planning prompt's list of read-only tools becomes `read, list_dir, glob, grep, lsp__hover, lsp__find_references, MCP read tools`. Add one shared sentence to all three prompts: *"Find code with grep/glob before reading; read only the lines you need."* It is fixed text, so the prefix stays stable.

### 8. Config

`[search] max_file_bytes = 2_000_000`, `timeout_ms = 10000` (the timeout is used in task-31). Env `ZCODE_SEARCH_MAX_FILE_BYTES`.

## Tests

`infra-search`:
- `grep_files_mode_counts_per_file`; `grep_content_mode_with_context`; `grep_literal_vs_regex`; `grep_smart_case`.
- `grep_bad_regex_is_pattern_error` (message names the problem position).
- `grep_skips_binary_and_large_files_and_counts_them`.
- `grep_type_filter_rust_only`; `grep_glob_filter`.
- `grep_is_deterministic_under_parallelism` (20 runs, 8 threads, byte-equal after rendering).
- `grep_stops_collecting_after_limit_but_keeps_totals` (fixture with 10 000 generated matches built at test time in a tempdir).
- `grep_cancel_returns_partial_quickly` (cancel closure flips after the first file; returns < 100 ms).
- `grep_catastrophic_regex_is_bounded` (`(a+)+$` on a long `aaaa…b` line: returns without hanging; ripgrep's engine has no backtracking, so this is a regression guard).
- `glob_patterns_sort_and_paging`.

`tools`:
- Snapshot tests of the rendered output for `files`, `content`, `count`, empty, capped (footer with `next: offset`), and `list_dir` depth 1/3 with collapse and excluded entries.
- `clip_line_centres_on_match_and_respects_char_boundaries` (multibyte text).
- `tool_order_is_fixed` (snapshot of `registry.list()` names).

Bench (`benches/`, criterion): `grep_vs_rg` on a generated tree of 100 k small files (created once under `target/bench-fixtures/`). `rg` timing is taken with `std::process::Command` when `rg` is on PATH, and skipped otherwise. Record the ratio in the PR (NFR-CTX-PERF-01: ≤ 1.5×).

## Test-case scenario

`zcode run --mode planning "where is the retry backoff for 429s computed?"` → the model calls `grep pattern="rate_limit_backoff" output=files` → 3 files → `grep … output=content path=crates/infra/llm/src/lib.rs context=2` → answers **without** a whole-file `read`. The ledger shows `locate` tokens < 1 k.

## How to verify

```sh
cargo test -p infra-search
cargo test -p tools grep glob list_dir
cargo bench -p zcode-benches grep_vs_rg
make size
make ci
```

**Pass criteria:** all tests green; planning mode advertises and dispatches `grep` and `glob`; bench ratio ≤ 1.5×; binary delta recorded; the evaluation run shows the `locate`+`discover` share falling on locate-and-explain tasks.

## Success metric mapping

M3 (discovery tokens), GR1 on planning-mode tasks; the leading indicator "search via `grep` rather than `shell`". FR-SEARCH-01..07, FR-GLOB-01..02.
