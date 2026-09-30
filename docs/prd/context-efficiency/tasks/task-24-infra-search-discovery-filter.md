# Task 24 — `infra-search` Crate: DiscoveryFilter, Default Excludes, Secret Hygiene

**Related PRD sections:** §5.5 FR-FILTER-01..05; Appendix C; §9.3 NFR-CTX-SEC-01
**Technical plan:** CE-DQ1, CE-DQ2, §3, §4, §5.2, §7.1
**Depends on:** — (Phase 1 can start in parallel with task-23)
**Phase:** 1 (v0.7.0)
**Status:** Done (v0.7.0)
**Priority:** High. `grep`, `glob`, `list_dir` (task-25) and the index (task-32) all go through this filter.

## Objective

Create `crates/infra/search` (package `infra-search`) with the `SearchPort` domain types and **one** `DiscoveryFilter`, the single authority on what discovery sees (PRD D-2). This task delivers the filter and the walk primitives (`walk_files`, `list`). Task-25 adds `grep` and `glob` on top.

## Step-by-step

### 1. Toolchain resolution (CE-DQ1)

`.cargo/config.toml`, add:

```toml
[resolver]
incompatible-rust-versions = "fallback"   # CE-DQ1: prefer releases whose declared MSRV fits 1.85
```

Workspace `Cargo.toml` `[workspace.dependencies]`:

```toml
# CE-DQ1: 0.4.30 uses let-chains (E0658 on 1.85) without declaring rust-version; 0.4.31+ declare 1.88.
ignore = "=0.4.29"
globset = "0.4"          # fallback resolver selects 0.4.19 (0.4.20 declares 1.88)
grep-searcher = "0.1"
grep-regex = "0.1"
grep-matcher = "0.1"
```

### 2. Domain types (`crates/domain/src/search.rs`, new; re-export from `lib.rs`)

`GrepQuery`, `GrepFileHit`, `GrepLine`, `GrepOutcome`, `GlobQuery`, `WalkEntry`, `EntryKind { File, Dir, Any }`, `CaseMode { Smart, Sensitive, Insensitive }`, and `trait SearchPort: Send + Sync` exactly as technical plan §5.2. This task implements `list` and `walk_files`. `grep` and `glob` return `Err("not implemented")` until task-25 (they are not registered as tools yet, so no model can reach them).

### 3. Crate skeleton

```
crates/infra/search/
  Cargo.toml        # domain, ignore, globset, grep-*, thiserror, log; dev: tempfile
  src/lib.rs        # RipgrepSearch: SearchPort; #![forbid(unsafe_code)] #![deny(clippy::unwrap_used)]
  src/defaults.rs   # DEFAULT_EXCLUDES, SECRET_EXCLUDES, SECRET_ALLOW (PRD Appendix C, verbatim)
  src/filter.rs     # DiscoveryFilter
  src/error.rs      # SearchError (thiserror), boxed at the port boundary
  testdata/repo/    # fixture tree (see Tests)
```

Add `infra-search` to `docs/architecture/dependency-check.sh` `INFRA_CRATES`, and as a dependency of `tools` and `cli`.

### 4. `FilterConfig` and `DiscoveryFilter`

```rust
pub struct FilterConfig { pub exclude: Vec<String>, pub include: Vec<String> }

pub struct DiscoveryFilter { root: PathBuf, overrides: ignore::overrides::Override, cfg: FilterConfig }

impl DiscoveryFilter {
    pub fn new(root: &Path, cfg: FilterConfig) -> Result<Self, SearchError>;
    /// A WalkBuilder rooted at `start`, with every layer applied (FR-FILTER-01).
    pub fn walker(&self, start: &Path) -> ignore::WalkBuilder;
    /// Why `path` is excluded, if it is (feeds `zcode ignore check`, FR-FILTER-07).
    pub fn explain(&self, path: &Path) -> Option<Exclusion>;
    pub fn is_excluded(&self, path: &Path, is_dir: bool) -> bool;
}
```

`walker(start)`:

```rust
let mut b = ignore::WalkBuilder::new(start);
b.hidden(false)                // FR-FILTER-04: dotfiles are visible
 .parents(true).ignore(true).git_ignore(true).git_global(true).git_exclude(true)
 .require_git(false)           // FR-FILTER-01: .gitignore honoured outside a repo
 .follow_links(false)          // FR-SEARCH-07
 .add_custom_ignore_filename(".zcodeignore")
 .overrides(self.overrides_for(start));
```

Override construction (`ignore::overrides::OverrideBuilder`, rooted at the project root). Override globs are *whitelist* by default, and `!glob` means ignore:

- For each default and secret exclude: `!<glob>` (directories as `!**/node_modules/**` **and** `!**/node_modules`, so the entry itself is skipped).
- For each `SECRET_ALLOW`: a whitelist glob such as `**/.env.example`.
- For each `cfg.exclude`: `!<glob>`. For each `cfg.include`: `<glob>`.
- Important: an `Override` containing *any* whitelist glob makes everything else non-matching-means-ignored. To avoid that, overrides hold only `!` rules, and the **whitelists (include + secret allow) are applied in `filter_entry`** instead: `b.filter_entry(move |e| !excluded_by_defaults(e) || whitelisted(e))`. Keep one code path: `is_excluded()` is the function `filter_entry` calls, and it is unit-tested directly.

**Explicit roots (FR-FILTER-02):** `overrides_for(start)` drops every default or config exclude rule that matches `start` itself or one of its ancestors below `root`. So `walker("node_modules/react")` walks the package, while `.git` *inside* it is still skipped. `.gitignore` rules are *not* dropped: a user-ignored directory named explicitly is still walked, because `ignore` evaluates gitignore relative to each directory it enters. The named root itself is never tested against its parent's gitignore. Test both behaviours.

### 5. `list(root, depth)` and `walk_files(root)`

- `list`: `walker(root).max_depth(Some(depth))`, collecting `WalkEntry { path (relative to project root, `/`-separated), is_dir, modified_ns, excluded: false }`. **Excluded directories directly under a listed directory** are then added with `excluded: true` (FR-FILTER-05): read that directory with `std::fs::read_dir`, and for each child directory where `is_excluded` is true, push an entry. They are never descended into. Sort by path (byte order).
- `walk_files`: files only, unbounded depth, sorted by path. Used by the index (task-32). Heuristic filters come in task-32.

### 6. Config plumbing (`infra-config`)

Add `[context] exclude = []`, `include = []` (PRD §7) to `Config` and the file mirror, merged per layer. **exclude and include accumulate across layers** (like `shell_denied`), so a machine-wide exclude cannot be dropped by a project file. `zcode config` prints both.

## Tests

Fixture `testdata/repo/` (committed; the `.git` dir is created at test time, because git cannot commit a nested `.git`):

```
.gitignore            -> "build/\n*.log\n!keep.log\n"
.zcodeignore          -> "secret-notes/\n"
.env  .env.example  .github/workflows/ci.yml
node_modules/react/index.js  node_modules/react/.git/HEAD
src/main.rs  src/app.log  src/keep.log  build/out.bin  target/debug/x
vendor/lib.go  secret-notes/a.md  id_rsa  config/.npmrc
```

- `defaults_exclude_node_modules_target_git`.
- `gitignore_applies_outside_a_git_repo` (no `.git` present).
- `gitignore_negation_readmits` (`keep.log` visible, `app.log` hidden).
- `zcodeignore_is_honoured`.
- `dotfiles_are_visible` (`.github/workflows/ci.yml`).
- `secrets_are_excluded_but_examples_are_not`.
- `config_exclude_and_include_layer_correctly` (`include = ["vendor/**"]` re-admits vendor).
- `explicit_root_inside_an_excluded_dir_is_walked` (`node_modules/react` lists `index.js` and not `.git`).
- `list_marks_excluded_children_collapsed` (`node_modules/ (excluded)` entry present, contents absent).
- `symlink_loops_do_not_hang` (create one at test time; unix only, `#[cfg(unix)]`).
- `output_is_sorted_and_deterministic` (20 runs).
- `explain_names_the_rule_and_source`.

## Test-case scenario

In a Next.js repo with `node_modules/` (40 k files) and `.next/`: `list(".", 1)` returns about 20 entries, with `node_modules/` and `.next/` shown as excluded, in < 50 ms.

## How to verify

```sh
cargo +1.85.0 build -p infra-search
cargo test -p infra-search
make check-arch
make size        # record the delta in the PR (expected ≈ +1.5 MB once grep lands in task-25)
make ci
```

**Pass criteria:** builds on the pinned 1.85 toolchain; all filter tests green; `check-arch` lists `infra-search` with no upward dependency; the `ignore` pin carries its `CE-DQ1` comment.

## Success metric mapping

FR-FILTER-01..05, NFR-CTX-SEC-01. Groundwork for M3 (discovery tokens).
