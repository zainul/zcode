# Task 34 — `edit_symbol`: AST-Located Edits with Syntax Gate and Re-Indentation

**Related PRD sections:** §5.6 FR-EDIT-01..05, 09; §6; §9.3 NFR-CTX-SEC-03; §12 R6
**Technical plan:** CE-DQ2, CE-DQ13, CE-DQ15, §5.5, §8 (`edit_symbol.rs`), §11 rule 3
**Depends on:** task-32 (`parse_text`, spans with attached trivia), task-27 (`after_write`, seams renderer), task-30 (LSP sync listener)
**Phase:** 3 (v0.9.0)
**Status:** Done (v0.7.0)
**Priority:** High. Makes edit payloads proportional to the change, with no preceding full read.

## Objective

A write-class tool that edits a definition **by name**: replace it, replace only its body, insert before or after it, or delete it. The target is located by parsing the **current** file content (never from stored spans; PRD R6). The edit is rejected if it introduces new syntax errors, and the result shows only the seams.

## Step-by-step

### 1. Mode gating

`domain::modes::write_tool_names()` adds `"edit_symbol"`. Tests: `edit_symbol_denied_in_planning`, `edit_symbol_allowed_in_editing_and_auto`, `edit_symbol_hidden_from_planning_specs`.

### 2. Tool schema

```
name: edit_symbol
description: "Edit a definition by name without sending the rest of the file. action: replace (whole definition incl. its
 doc comments/attributes), replace_body (keep the signature), insert_before, insert_after, delete. The file is re-parsed
 and the edit is refused if it adds syntax errors."
params: path*, symbol* (e.g. "AgentLoop::execute", "UserService.create", "handler"), action*, content, line (disambiguation)
```

### 3. Algorithm (`tools/src/edit_symbol.rs`)

```
1. text  = read(path)                                  (missing → model error)
2. before = index.parse_text(path, text)               (None → error "edit_symbol supports rust, go, typescript, python; use str_replace_editor")
3. matches = before.defs where qualified == symbol || (no separator in symbol && name == symbol)
             filtered by `line` (the span contains the line) when given
   0 → error listing up to 10 nearest names (edit distance ≤ 3 or same last segment)   FR-EDIT-03
   >1 → error "symbol is ambiguous: …" with `kind qualified — L…` per candidate; suggest `line`
4. target span:
   replace / delete        → def.span (including attached docs/attrs, task-32)
   replace_body            → the body node's byte range: the `body` field of the def node (block / statement_block /
                             declaration_list / field_declaration_list …). No body (e.g. a trait method signature)
                             → error "X has no body; use replace"
   insert_before / after   → an empty range at span.start (start of line) / after span.end (end of line)
5. content = reindent(content, target_indent(text, span), style = detect(text))       FR-EDIT-05
   replace_body: the new body must include its braces/colon block for brace languages; if the content does not start
                 with `{` (Rust/Go/TS), wrap it: "{\n" + reindent(content, indent+1) + "\n" + indent + "}".
                 Python: the content is the indented block.
   insert_*: ensure exactly one blank line separates the insertion from the neighbouring definition.
   delete: also remove one adjacent blank line so the file does not accumulate gaps.
6. new_text = splice(text, range, content)
7. after = index.parse_text(path, new_text)
   if after.error_nodes > before.error_nodes && syntax_check == reject → error:
      "edit refused: it introduces a syntax error at L123:9 (unexpected `}`). Nothing was written."   FR-EDIT-04
   warn → write, and append that line as a warning
8. fs.write_atomic; after_write(path, new_text)       → index notify_changed + LSP didChange (FR-EDIT-09)
9. result: "replaced fn AgentLoop::execute  L386-819 → L386-801" + seams(±2 lines)   FR-EDIT-02
   subject = FileWrite { path }
   (+ diagnostics delta appended by task-35, FR-LSP-08)
```

### 4. Indentation (`reindent`)

- `detect(text)`: tabs if more lines start with `\t` than with spaces. Otherwise the space width = the GCD of the leading-space counts of indented lines (clamped to 2..=8, default 4).
- `target_indent`: the leading whitespace of the span's first line.
- `reindent(content, indent)`: when the content's own minimum indentation is 0 (the model sent it flush-left), prefix each non-empty line with `indent`, converting the content's own indentation units to the file's style. When it already carries indentation, strip the common minimum and apply the target (so both forms work). A trailing newline is normalised.

### 5. Byte and line safety

Splicing uses tree-sitter byte offsets on the **same** `text` that was parsed (step 2), so the offsets are always valid. All slicing goes through `str::get(range)` → `Result` (no panics; `#![deny(clippy::unwrap_used)]` in this module). CRLF files keep CRLF: `content` is converted to the file's detected newline style before the splice.

### 6. Registration

`ToolRegistry::from_config`: register `EditSymbolTool` only when a code index is available (`--no-index` → not advertised, so the model is not offered a tool that would always fail). Order per technical plan §8.

## Tests

Golden tests per language (Rust, Go, TS, Python) for each action:

- `replace_includes_doc_comments_and_attributes` (Rust `///` + `#[inline]`, Python decorator, TS JSDoc).
- `replace_body_keeps_signature`; `replace_body_without_braces_is_wrapped` (Rust/Go/TS); `replace_body_python_block`.
- `insert_after_adds_single_blank_line`; `insert_before`; `delete_removes_trivia_and_one_blank_line`.
- `method_in_impl_by_qualified_name`; `go_method_by_receiver`; `ts_class_method`.
- `ambiguous_symbol_lists_candidates_and_writes_nothing`; `line_hint_disambiguates`.
- `missing_symbol_suggests_near_names`.
- `syntax_gate_rejects_new_error_with_location`; `syntax_gate_allows_edit_in_file_with_existing_error`; `syntax_check_warn_writes_and_warns`.
- `reindent_flush_left_tabs_and_spaces`; `reindent_preserves_relative_indentation`; `crlf_preserved`.
- `result_is_seams_only` (the result is < 12 lines for any golden case).
- `stale_index_does_not_misplace_edit` (modify the file on disk after indexing without notifying; the edit still hits the right span, because step 2 re-parses).
- `after_write_notifies_index_and_lsp` (fake listeners).
- `not_registered_without_index`.
- `unsupported_language_is_model_error`.

## Test-case scenario

"Make `truncate_tool_output` keep the tail as well." The model calls `outline crates/app/src/lib.rs`, then `edit_symbol path=crates/app/src/lib.rs symbol=truncate_tool_output action=replace_body content="…12 lines…"`. The payload is ~150 tokens and the result is 7 lines. v0.6 needed a full read (~12 k tokens for this file), plus `str_replace` with an exact old body, plus an echo of the edit.

## How to verify

```sh
cargo test -p tools edit_symbol
cargo test -p domain modes
make ci
make eval-tokens LIVE=1 LABEL=p3-edit
```

**Pass criteria:** tests green; the evaluation shows `change`-category tokens and edit-argument tokens ↓ ≥ 30 % vs Phase 2 (PRD §2.3 edit-payload indicator); share of edits made without a whole-file `write` ≥ 90 %; no increase in failed or reverted edits (GR1).

## Success metric mapping

M2, M3 (no pre-edit reads), GR1, leading indicators. FR-EDIT-01..05, 09. Completes the fix for B9.
