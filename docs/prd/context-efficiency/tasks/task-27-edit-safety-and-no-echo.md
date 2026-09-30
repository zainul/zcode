# Task 27 — Edit Safety: Ambiguity Errors, Forgiving Match with Disclosure, No-Echo Results

**Related PRD sections:** §5.6 FR-EDIT-06..08; §1.1 B9; §9.4 NFR-CTX-COMPAT-04
**Technical plan:** CE-DQ3, CE-DQ13, §8 (`native.rs`, `patch.rs`)
**Depends on:** task-21 (`subject`), task-25 (`render.rs`)
**Phase:** 1 (v0.7.0)
**Status:** Done (v0.7.0)
**Priority:** High. Removes the "read the whole file before every edit" loop, and a wrong-place edit hazard.

## Objective

Make the existing text-based edit tools cheap and safe:

1. `str_replace` **refuses** ambiguous matches (today it edits the first and reports the count afterwards), with `replace_all` and `expected_replacements` as explicit opt-ins.
2. When `old_str` is not found exactly, try **one** whitespace-normalised match and say so if it was used. Otherwise return the **closest region** with line numbers, so the model can retry without a full re-read.
3. No edit tool echoes file content. Results are diffstats plus the lines around each seam.
4. Every write reports `subject: FileWrite`, and calls one `after_write` hook (used by the index and LSP in later tasks).

## Step-by-step

### 1. `str_replace` rewrite (`StrReplaceTool::str_replace`)

New params: `replace_all` (bool, default false), `expected_replacements` (integer).

```
content = read(full)
positions = match_indices(content, old)
match positions.len():
  0  → try normalised(content, old)                       // step 2
  1  → apply
  n  → if replace_all → apply all
       else error "old_str matches {n} places (lines 12, 88, 140) — add surrounding lines to make it unique, or set replace_all"
if let Some(e) = expected_replacements && e != applied → error "expected {e} replacements, found {applied}; nothing written"
```

The line numbers come from counting `\n` before each byte offset (one pass).

### 2. Normalised retry and closest region (FR-EDIT-07)

```rust
fn normalise(line: &str) -> (usize /*indent width, tabs=4*/, &str /*trimmed both ends*/)
```

- Split `old` and `content` into lines. Find windows of `content` with `old.lines().count()` lines where every line's trimmed text is equal. Indentation *width* may differ by a constant offset across the window (treat that as the same block re-indented).
- Exactly one window → replace that window's byte range with `new`, re-indented by the same offset if `new` is at the old block's base indentation. The result gets ` (matched with whitespace normalisation)`.
- More than one → the ambiguity error from step 1.
- None → **closest region**: for each window position, score = the number of lines whose trimmed text equals the corresponding `old` line, with ties broken by earliest position. Return the best window (≤ 10 lines, with the gutter) in the error:

```
old_str not found in src/lib.rs. Closest match (4 of 6 lines equal), lines 118-123:
  118│    let x = compute(a,
  119│                    b);
  …
Copy the exact text from here, or read a wider range.
```

Complexity O(lines × window). Cap the search at files ≤ 20 000 lines. Above that, return the plain not-found error.

### 3. No-echo results (FR-EDIT-02-style, FR-EDIT-08)

`tools/src/render.rs` gets `fn seams(before: &str, after: &str, changed: &[(usize, usize)], ctx: usize) -> String`. For each changed line range in `after`, it shows `ctx = 2` lines before and after with the gutter, and a `…` between separate seams. At most 3 seams are shown, then `(+N more changes)`.

- `str_replace`: `edited src/lib.rs: 1 replacement (L118-123 → L118-125)` + seams.
- `write` on an **existing** file: `wrote src/lib.rs: 412 → 430 lines (+31 −13)` using a line-level LCS diff (a simple O(n·d) Myers implementation in `render.rs`, capped at 5,000 lines; above that, report the line counts only). If the old file had more than 200 lines, append: `tip: for small changes use str_replace_editor or edit_symbol — they send only the changed lines.`
- `write` on a new file: `created src/new.rs (57 lines)`.
- `apply_patch` (`patch.rs`): per file `M src/a.rs +12 −3`, `A src/new.rs +40`, `D src/old.rs`. No content.
- `create` (`str_replace_editor`): as for `write` on a new file.

### 4. `after_write` hook

`ToolRegistry` gets `fn after_write(&mut self, rel_path: &str, new_text: &str)`. In this task it is a no-op that the native write tools call through a shared `Arc<Mutex<Vec<Box<dyn FnMut(&str, &str) + Send>>>>` of listeners, registered by `from_config`. Task-30 registers LSP sync; task-32 registers index `notify_changed`. Every write path calls it: `write`, `str_replace_editor create|str_replace`, `apply_patch` (per file), and later `edit_symbol` and rename `apply`.

`subject = Some(Subject::FileWrite { path })` on every successful write result (for task-28 supersession).

### 5. Docs

- CHANGELOG *Changed*: "`str_replace_editor str_replace` now errors on ambiguous `old_str` instead of editing the first occurrence; pass `replace_all: true` to replace every occurrence." Include the reason (a wrong-place edit is silent until later).
- Tool description updates: one line each, within the schema budget.

## Tests

- `ambiguous_old_str_is_an_error_with_line_numbers_and_nothing_written`.
- `replace_all_replaces_every_occurrence`; `expected_replacements_mismatch_writes_nothing`.
- `indentation_drift_matches_with_disclosure` (tabs vs spaces, a 4-space shift).
- `trailing_whitespace_drift_matches`.
- `normalised_match_that_is_ambiguous_is_an_error`.
- `not_found_returns_closest_region_with_gutter`; `closest_region_is_skipped_for_huge_files`.
- `write_existing_reports_diffstat_and_tip_over_200_lines`; `write_new_reports_created`.
- `apply_patch_reports_per_file_diffstat_without_content`.
- `results_never_contain_the_full_file` (property: for random edits on a 300-line fixture, the result length is < 30 % of the file).
- `every_write_path_calls_after_write` (a listener records the paths).
- `write_results_carry_file_write_subject`.

## Test-case scenario

The model sends `str_replace` with `old_str` copied from an earlier read, but with 2-space indentation where the file uses 4. v0.6: `old_str not found`, then a full re-read (~6 k tokens), then a retry. Now: applied, with `(matched with whitespace normalisation)`, in one call.

## How to verify

```sh
cargo test -p tools str_replace write apply_patch after_write
make ci
```

**Pass criteria:** tests green; CHANGELOG entry present; the evaluation run shows fewer `read` calls immediately after a failed `str_replace` (from JSONL sequences) and lower `change`-category tokens.

## Success metric mapping

M2, M3; leading indicator "edits without a whole-file rewrite ≥ 90 %"; GR1 (fewer wrong-place edits). FR-EDIT-06..08. Resolves B9 partly (task-34 completes it).
