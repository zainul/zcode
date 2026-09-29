# Task 26 — Read Shaping: Ranges, Gutter, Large-File Guard, Head+Tail Truncation, Spill Files

**Related PRD sections:** §5.8 FR-READ-01..05, 07; §1.1 B1, B8; §4.2 principles 1–2
**Technical plan:** CE-DQ3, CE-DQ12, CE-DQ13, CE-DQ14, §6.1, §7.7, §8
**Depends on:** task-21 (`meta`, `subject`), task-25 (`render.rs`)
**Phase:** 1 (v0.7.0)
**Status:** Todo
**Priority:** High. Without ranged reads, cheaper search does not turn into cheaper reading.

## Objective

1. `read` (and `str_replace_editor view`) return **line ranges** with a **1-based gutter**, and guard large files.
2. Tool output over budget keeps **head 40 % + tail 60 %**, so the end of long output (compiler errors, test summaries) survives.
3. The full output of a truncated call is **spilled** to `.zcode/spill/…`, and the marker says where, so the model can `grep`/`read` the rest.

## Step-by-step

### 1. `ReadTool` (`tools/src/native.rs`)

Params: `path`*, `offset` (1-based start, default 1), `limit` (lines, default `read.default_limit` = 400).

```
read(path, offset, limit):
  bytes = fs.read_bytes(full)                       // new StdFs helper; String read fails on binary
  if looks_binary(bytes[..8192]) → error "binary file (N bytes, <kind>) — not shown"   // FR-READ-04
  text = from_utf8_lossy
  lines = text.lines() (keep a trailing-newline flag)
  total = lines.len()
  if offset > total && total > 0 → error "offset 900 is past the end (file has 412 lines)"
  end = min(offset + limit - 1, total)
  body = for n in offset..=end: gutter(n, width(end)) + clip_long(line, 2000)   // FR-READ-03/04
  if offset == 1 && end < total && no explicit limit/offset given:              // FR-READ-02
       body += "\n" + outline_hint(path)   // outline from CodeIndexPort when set (task-33); omitted until then
       footer "[lines 1-400 of 2,318 — use offset/limit, or grep/outline to find the part you need]"
  else if end < total: footer "[lines {offset}-{end} of {total}; next: offset {end+1}]"
  subject = FileRange { path: rel, start: offset, end, hash: fnv64(text) }
```

- `clip_long`: lines > 2,000 chars → first 2,000 chars + `…[+N chars]` (char-boundary safe).
- The gutter width is the digit count of `end`, so a 99-line range pays 2 cells.
- `looks_binary`: a NUL byte in the first 8 KB (the same rule as ripgrep).
- An empty file → `(empty file)`.

The spec description mentions ranges and the gutter in one sentence.

### 2. `str_replace_editor view`

Accept `view_range: [start, end]` (the Anthropic editor convention; `end = -1` means EOF) and map it to the same function. Its `subject` is `FileRange` too, and `domain::tool_category` classifies `str_replace_editor` by argument. Add `tool_category_for_call(name, args_json_has_view: bool)`: the engine detects `"command":"view"` with a cheap substring check on the raw arguments (no JSON parsing in `domain`).

### 3. Line-number contract test (FR-READ-03)

`tools`: `a_grep_line_number_addresses_the_same_line_in_read`. `grep` gives `N`; `read offset=N limit=1` returns that exact line. (LSP joins this test in task-30.)

### 4. Head + tail shaping (`app/src/lib.rs`)

Replace `truncate_tool_output` with:

```rust
pub fn shape_tool_output(content: String, max_chars: usize) -> Shaped {
    if content.len() <= max_chars { return Shaped { text: content, cut: None } }
    let head_budget = max_chars * 2 / 5;           // 40 %
    let tail_budget = max_chars - head_budget;     // 60 %
    let head_end   = floor_char_boundary(&content, head_budget) → then back to the last '\n' if one is within 200 bytes
    let tail_start = ceil_char_boundary(&content, content.len() - tail_budget) → forward to the next '\n' if within 200 bytes
    let omitted_lines = content[head_end..tail_start].matches('\n').count();
    text = head + "\n…[omitted {omitted_lines} lines / {omitted_chars} chars{SPILL}]…\n" + tail
}
```

`truncate_tool_output` stays as a thin wrapper returning `(String, bool)` for back-compat. Update its existing tests: `tool_output_is_capped_before_entering_history` now asserts that the **tail** survives and that the marker is present.

### 5. Spill (CE-DQ12)

- `domain/src/ports.rs`: `pub trait SpillPort { fn spill(&mut self, session: &str, call_id: &str, content: &str) -> Result<String, BoxError>; }`
- `infra-filesystem/src/spill.rs`: `SpillStore::new(working_dir: &Path, ttl_days: u32)`. On construction it prunes `.zcode/spill/<sid>/` directories whose mtime is older than the TTL, ignoring errors (logs a warning). `spill()` writes `.zcode/spill/<sid>/<sanitised call id>.txt` with `write_atomic` and returns the working-dir-relative path. Call ids are sanitised to `[A-Za-z0-9_-]`, max 64 chars.
- `App::set_spill(Box<dyn SpillPort + Send>)`. In dispatch: when `Shaped.cut` is `Some` **and** a spill store is set, spill the *unshaped* content first, then build the marker with `; full output: <path>`. A spill failure → marker without a path, plus a `log::warn` through the logger port. The run never fails.
- `meta.spill = Some(path)`. Telemetry `spilled: true`.
- `cli::wire`: `SpillStore::new(&cfg.working_dir, cfg.context.spill_ttl_days)` (default 7). Config key `[context] spill_ttl_days`.
- zcode has no `session delete` command (`SessionCmd` is create/continue/fork/import/export), so the TTL prune is the only cleanup. That is deliberate: spill files are cheap, and a spill path stays valid for as long as a resumed session could still reference it (7 days by default).

### 6. Budgets

This task keeps the single `max_tool_output_chars` (default 32 000) as the budget. Per-tool budgets (FR-READ-06) come in task-31.

## Tests

- `read_range_returns_gutter_and_footer`; `read_offset_past_end_is_model_error`; `read_default_large_file_guard_footer` (2,000-line fixture generated at test time).
- `read_refuses_binary`; `read_clips_long_lines_on_char_boundary`; `read_empty_file`.
- `view_range_maps_to_read` (including `-1`).
- `read_sets_file_range_subject_with_content_hash`.
- `shape_keeps_head_and_tail_and_counts_omitted_lines`; `shape_never_splits_a_char` (multibyte fuzz over a fixed seed); `shape_under_budget_is_identity`.
- `a_cargo_error_at_the_end_survives_truncation` (100 k-char synthetic cargo output ending in `error[E0308]`).
- `spill_writes_full_output_and_marker_names_it` (tempdir); `spill_failure_does_not_fail_the_run` (fake `SpillPort` returning `Err`); `spill_store_prunes_expired_dirs`; `spill_sanitises_call_ids`.

## Test-case scenario

`zcode run "fix the failing test in crates/tools"`. `shell cargo test -p tools` prints 90 k chars; the result keeps the start and the failing-test summary, and says `full output: .zcode/spill/<sid>/call_7.txt`. The model then runs `grep pattern="panicked at" path=.zcode/spill/<sid>/call_7.txt output=content` instead of re-running the tests. (`.zcode/` is excluded from discovery, but an explicit path is served; FR-FILTER-02.)

## How to verify

```sh
cargo test -p tools read
cargo test -p app shape spill
cargo test -p infra-filesystem spill
make ci
```

**Pass criteria:** all tests green; the ledger's median `read` size on the evaluation corpus falls (leading indicator); no task in the corpus re-runs a build command immediately after a truncated build result (checked from the evaluation JSONL).

## Success metric mapping

M2, M3 (read tokens), GR2 (fewer re-runs). FR-READ-01..05, 07. Resolves B1 and B8.
