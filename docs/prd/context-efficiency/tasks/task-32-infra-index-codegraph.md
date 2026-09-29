# Task 32 — `infra-index` Crate: tree-sitter Extraction, Persistence, Background Build, Freshness

**Related PRD sections:** §5.4 FR-INDEX-01..04, 09 (engine side), 10, 11; §5.5 FR-FILTER-06; §9.1 NFR-CTX-PERF-02/03, MEM-02, SIZE-01; §12 R5, R6
**Technical plan:** CE-DQ1, CE-DQ2, CE-DQ15, CE-DQ16, CE-DQ17, §3, §5.2, §7.2, §11
**Depends on:** task-24 (`SearchPort::walk_files`, DiscoveryFilter)
**Phase:** 3 (v0.9.0)
**Status:** Todo
**Priority:** High. The foundation for `outline`, `symbols`, `related`, the repo map, `edit_symbol`, and index-backed LSP addressing.

## Objective

Create `crates/infra/index` (package `infra-index`), implementing `domain::CodeIndexPort`:

- tree-sitter parsing with zcode-owned queries (CE-DQ15) for Rust, Go, TypeScript/TSX and Python, each behind a cargo feature (CE-DQ16).
- A background build on a `std::thread` that never delays the first model request (FR-INDEX-01).
- An incremental, versioned, custom-format store under `.zcode/index/v1/` (CE-DQ17).
- Freshness on zcode's own writes and on external edits (FR-INDEX-04).
- Resource bounds: file size cap, parse timeout, memory budget (FR-INDEX-11).

Graph ranking and the repo map are task-33. This task stores the occurrence and import data they need.

## Step-by-step

### 1. Dependencies (verified on 1.85 in technical plan §3)

```toml
# crates/infra/index/Cargo.toml
[dependencies]
domain = { path = "../../domain" }
infra-search = { path = "../search" }
tree-sitter = "0.26"                 # CE-DQ1: 0.27 needs 1.90; fallback resolver → 0.26.13
streaming-iterator = "0.1"
tree-sitter-rust = { version = "0.24", optional = true }
tree-sitter-go = { version = "0.25", optional = true }
tree-sitter-typescript = { version = "0.23", optional = true }
tree-sitter-python = { version = "0.25", optional = true }
thiserror = { workspace = true }
log = { workspace = true }

[features]
default = ["lang-rust", "lang-go", "lang-typescript", "lang-python"]
lang-rust = ["dep:tree-sitter-rust"]
lang-go = ["dep:tree-sitter-go"]
lang-typescript = ["dep:tree-sitter-typescript"]
lang-python = ["dep:tree-sitter-python"]
```

`tools` gets `code-index = ["dep:infra-index"]` (default on), and `cli` forwards it. Add `infra-index` to `dependency-check.sh`.

### 2. Domain (`crates/domain/src/code_index.rs`)

Types and trait exactly as technical plan §5.2 (`SymbolKind`, `Span`, `SymbolDef`, `IndexState`, `Related`, `ParsedFile`, `CodeIndexPort`). `Related { imports, imported_by, defined_in, referenced_in: Box<[(String, u32)]> }` (path, line; approximate).

### 3. Languages and queries

`src/lang.rs`: `enum Lang { Rust, Go, TypeScript, Tsx, Python }`, `Lang::for_path(&str) -> Option<Lang>` (by extension: `rs`, `go`, `ts`/`mts`/`cts`, `tsx`, `py`/`pyi`; `.js`/`.jsx` map to TypeScript/TSX, because the TS grammar parses modern JS well enough for definitions). Each variant is `#[cfg(feature = …)]`, and `for_path` returns `None` for a disabled language.

`queries/<lang>/defs.scm`. Capture conventions (all languages):

| Capture | Meaning |
|---------|---------|
| `@def.function` `@def.method` `@def.struct` `@def.enum` `@def.trait` `@def.interface` `@def.class` `@def.type` `@def.const` `@def.module` | the definition node |
| `@name` | its name identifier (goes into `SymbolDef.name`, `name_line`, `name_col`) |

Example (Rust, excerpt):

```scheme
(function_item name: (identifier) @name) @def.function
(impl_item body: (declaration_list (function_item name: (identifier) @name) @def.method))
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(type_item name: (type_identifier) @name) @def.type
(const_item name: (identifier) @name) @def.const
(mod_item name: (identifier) @name) @def.module
```

`queries/<lang>/imports.scm` (`@import.path`) and `queries/<lang>/idents.scm` (`@ident`: identifier references, excluding definitions' `@name` nodes).

Attached leading trivia (for `edit_symbol` spans, task-34): implemented in code, not queries. Walk `prev_named_sibling` while the sibling is a comment (`line_comment`, `block_comment`, `comment`), an `attribute_item` (Rust) or a `decorator` (Python/TS), **and** it ends on the line directly above the current start (no blank line in between). The first such sibling's start becomes `span.start_*`.

Qualified names (CE-DQ15): walk ancestors and prepend container names. Rust: `impl_item` → type name (`impl Foo` / `impl Trait for Foo` → `Foo`), `trait_item`, `mod_item`. Go: the method receiver type (`func (s *Server) Run` → `Server.Run`). TS: `class_declaration`, `interface_declaration`, `internal_module`. Python: `class_definition`. Separators: `::` for Rust, `.` for the others.

### 4. `src/extract.rs`

```rust
pub fn parse(lang: Lang, text: &str, budget: Duration) -> Result<ParsedFile, IndexError>
```

- `Parser::set_language`; `parse_with_options(&mut |i, _| &bytes[i..], None, Some(ParseOptions::new().progress_callback(&mut |_| if started.elapsed() > budget { ControlFlow::Break(()) } else { ControlFlow::Continue(()) })))`. A `None` tree → `IndexError::Timeout` → the file is recorded as `skipped: timeout` (FR-INDEX-11).
- Run `defs` → `SymbolDef`s (signature = the text of the definition's first line from the name's line, trimmed, clipped to 200 chars; `depth` = the number of qualified-name segments − 1).
- `error_nodes`: a tree walk counting `node.is_error() || node.is_missing()`; `first_error` = the (line, col) of the first one (1-based).
- Queries are compiled **once per language** and cached in a `OnceLock<Query>` per language.

### 5. `src/store.rs` (CE-DQ17)

```
header:  b"ZCIX" | u16 format_version=1 | u32 file_count | u32 string_count
files:   for each: u32 path_sid | u64 mtime_ns | u64 size | u64 fnv1a | u8 lang | u8 flags(skipped?)
         u32 n_defs  { u32 name_sid u32 qual_sid u8 kind u32 start_line u32 end_line u32 start_byte u32 end_byte
                       u32 name_line u32 name_col u32 sig_sid u8 depth }
         u32 n_imports { u32 spec_sid u32 resolved_path_sid_or_MAX }
         u32 n_idents  { u32 name_sid u32 line }                       // deduplicated per (name, line)
strings: for each: u32 len | bytes (UTF-8)
```

- All integers little-endian, read through a bounds-checked cursor (`Result`, never a panic; plan §11 rule 3).
- `save(path)`: write `index.zcix.tmp`, `sync_data`, rename.
- `load(path)`: any error or version mismatch → `Ok(None)` + `log::warn!("index discarded: …; rebuilding")`.

### 6. `src/builder.rs`: background build

```rust
pub fn spawn(root: PathBuf, search: Arc<dyn SearchPort + Send + Sync>, cfg: IndexConfig) -> Arc<CodeIndex>
```

1. Create `CodeIndex { state: RwLock<Snapshot>, progress: AtomicU32s, … }` in state `Building{0, 0}` and return the `Arc` **immediately**.
2. A thread named `zcode-index`, with `std::thread::Builder`: load the store → `walk_files(root)` (FR-FILTER-06 heuristics applied here: skip binary, minified, generated, lockfiles, and files > `index.max_file_bytes`) → for each file, reuse the stored record when `(mtime, size)` matches, otherwise read it, compute FNV-1a and reuse when the hash matches, otherwise parse.
3. Publish into the `RwLock` every 200 files (writers hold the lock only for the swap of a prepared `Vec`), so early queries see partial data.
4. At the end: build the name index `by_name: HashMap<Box<str>, Vec<(file_idx, def_idx)>>`, set `Ready`, save the store, and emit `index_ready` through a callback the CLI turns into telemetry.
5. Thread priority: none (std has no API). Instead, parse in chunks and `std::thread::yield_now()` between files.

`IndexConfig { enabled, max_file_bytes, parse_budget: 500ms, store_path: <wd>/.zcode/index/v1/index.zcix }`.

### 7. Heuristics (`infra-search/src/heuristics.rs`, FR-FILTER-06)

`looks_binary(first_8k)`, `looks_minified(text)` (median line length > 300, or any line > 5,000 chars), `looks_generated(first_5_lines)` (contains `@generated`, `DO NOT EDIT`, `Code generated … DO NOT EDIT` (Go convention), or `autogenerated`), `is_lockfile(name)`. Exposed through `SearchPort::walk_files_for_index` (a new trait method with a default of `walk_files`).

### 8. `src/lib.rs`: `CodeIndex: CodeIndexPort`

- `outline(path)`: `None` for an unsupported language. For a directory: the top-level defs (`depth == 0`) of each supported file directly inside it, ordered by path.
- `symbols(query, kind, prefix, limit)`: candidates from `by_name` (exact, then prefix by scanning sorted keys with `range`), then fuzzy subsequence over names (cap 50 k comparisons). Ranking: exact > prefix > subsequence, then the graph rank (task-33; until then, path order), then path/line. Deterministic.
- `locate(path, qualified)`: an exact `qualified` match, else a match on the last segment, filtered by `path` if given.
- `parse_text(path, text)`: parses directly (no store), for `edit_symbol` (PRD R6: never trust stored spans for edits).
- `notify_changed(path)`: synchronous re-parse of that file and an in-place replace under the write lock. The store is saved lazily (a dirty flag, saved on `Drop` and at most every 30 s).
- **Freshness on query** (FR-INDEX-04): each query `stat`s the files it is about to return, and re-parses any whose `(mtime, size)` changed.
- A stat-only rescan at turn start: `CodeIndex::rescan_changed()` walks and stats (no reads) and re-parses changed or new files. It is called by the engine through `CodeIndexPort::notify_turn_start()` (a new defaulted trait method), in a background thread if the repo has more than 5 k files.
- `heap_bytes()` for the memory test.

### 9. Wiring and CLI (FR-INDEX-10)

- `cli::wire`: unless `--no-index` / `index.enabled = false`, `let index = infra_index::spawn(...)`. Pass it to `ToolRegistry::with_code_index` and `App::set_code_index`.
- `zcode index status`: loads the store without building, and prints files, symbols, languages, skipped counts by reason, store size, age.
- `zcode index rebuild`: deletes the store and runs a foreground build with a progress line.
- `zcode index clear`.

## Tests

Fixtures `testdata/{rust,go,ts,tsx,py}/` with known definitions, doc comments, attributes and decorators, nested containers, and one file with a syntax error.

- `extracts_expected_defs_per_language` (golden list: qualified name, kind, span, signature).
- `spans_include_attached_docs_and_attributes_but_not_after_blank_line`.
- `go_method_qualified_by_receiver`; `rust_impl_trait_for_type_qualifies_by_type`.
- `error_nodes_counted_with_first_location`.
- `parse_timeout_is_skipped_not_hung` (a pathological 5 MB nested-brackets file generated at test time, with a 50 ms budget).
- `store_round_trip`; `corrupt_store_is_discarded`; `version_mismatch_rebuilds`.
- `incremental_touch_reparses_one_file` (a parse counter).
- `queries_during_build_see_partial_results_and_state_building`.
- `notify_changed_updates_spans`; `external_edit_detected_on_query`.
- `heuristics_skip_minified_generated_lockfiles`.
- `unsupported_language_outline_is_none`.
- `disabled_features_compile` (CI step: `cargo check -p infra-index --no-default-features`).
- Memory: `index_heap_under_budget_for_10k_files` (a synthetic tree of 10 k small files generated in a tempdir; `#[ignore]` because it is slow, and run in the task PR): ≤ 25 MB.
- Bench (criterion): cold build of 10 k files ≤ 20 s; warm start ≤ 300 ms; `symbols` p95 ≤ 20 ms.

## How to verify

```sh
cargo test -p infra-index
cargo check -p infra-index --no-default-features
cargo test -p infra-index -- --ignored index_heap_under_budget_for_10k_files
cargo bench -p zcode-benches index
make size          # record the per-grammar delta: build with each lang feature alone
make check-arch
make ci
```

**Pass criteria:** tests green on toolchain 1.85; `zcode version` cold start unchanged (GR4: the build is never on that path, because `version` returns before `wire`); binary delta within NFR-CTX-SIZE-01 (≤ +6 MB overall, with the per-grammar breakdown recorded in the PR); benches within NFR-CTX-PERF-02/03.

## Success metric mapping

Enables M3 via task-33 and task-34. FR-INDEX-01..04, 09 (engine side), 10, 11; FR-FILTER-06; NFR-CTX-MEM-02, SIZE-01.
