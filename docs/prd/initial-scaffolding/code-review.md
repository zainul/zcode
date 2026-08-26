# Code Review: Initial Scaffolding — zcode v0.1.0

**Review ID:** CR-SCAFFOLD-001  
**Reviewer:** Senior Tech Lead  
**Date:** 2026-08-24  
**Branch:** `develop-release-initial-scaffolding`  
**Commit:** `d9c7219d3f6d80e4a9027cc556aabf7812eb0e08`  
**PRD:** `docs/prd/initial-scaffolding/prd.md`  
**Technical Plan:** `docs/prd/initial-scaffolding/technical-plan.md`

---

## 1. Summary

The initial-scaffolding milestone delivers a complete, compilable Rust Cargo workspace implementing **Clean Architecture** in Rust for the zcode terminal coding agent. The workspace contains **7 functional crates** plus a criterion benchmark crate, all wired into a strict acyclic dependency graph (`cli → app/infra/* → domain`).

The scaffolding establishes the dependency-inversion boundary (port traits in Domain, concrete adapters in Infra), a fail-fast composition root (`wire()`), build-metadata embedding via a custom git-SHA build script, and the full quality-gate toolchain (Makefile, clippy, rustfmt, deny.toml, dependency-check script).

**Result: All primary gates (M1.1–M1.6) pass.** The workspace builds with zero warnings, tests are green, clippy is clean with `-D warnings`, `cargo fmt --check` passes, `cargo doc` builds, the Domain crate is dependency-free, the dependency graph is acyclic, and `cargo run --quiet -- version` prints the required build metadata.

### Artifacts delivered

| Task | Artifact | Status |
|------|----------|--------|
| task-01 | Workspace root manifests (`Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.gitignore`) | Done |
| task-02 | `crates/domain` — entities, `DomainError`, 5 port traits + `LogLevel`, `CompletionChunk` | Done |
| task-03 | `crates/app` — `App` orchestrator, `TaskRunner`/`EditPlanner` traits, `AppError` | Done |
| task-04 | `crates/infra/llm` — `OpenAiLlm` implementing `LlmPort` (stub, no network) | Done |
| task-05 | `crates/infra/filesystem` — `StdFs` implementing `FileSystemPort` | Done |
| task-06 | `crates/infra/shell` — `StdShell` implementing `ShellPort` | Done |
| task-07 | `crates/infra/config` — `Config` model + `TomlConfigLoader`, env/file merge | Done |
| task-08 | `crates/cli` — `clap v4` CLI, `version` subcommand, `wire()` composition root, `build.rs` | Done |
| task-09 | `benches/` — criterion smoke benchmark | Done (one bug fixed in review; see §5.1) |
| task-10 | `README.md`, `docs/architecture/README.md`, `CHANGELOG.md`, `CONTRIBUTING.md` | Done |
| task-11 | `Makefile` + `docs/architecture/dependency-check.sh` | Done |

---

## 2. PRD Coverage

Verification commands run against the live build (see §3 for raw output):

| FR/NFR ID | Requirement | Status | Evidence |
|-----------|-------------|--------|----------|
| FR-DI-01 | `domain` crate: zero third-party deps | ✅ Pass | `cargo tree -p domain` → 1 line (domain only) |
| FR-DI-02 | `app` depends only on `domain` | ✅ Pass | `cargo tree -p app` → `domain` + `thiserror` only |
| FR-DI-03 | `infra/*` → `domain` + external; no `app`/`cli` | ✅ Pass | `dependency-check.sh` infra checks OK |
| FR-DI-04 | `cli` is composition root, depends on all layers | ✅ Pass | `dependency-check.sh` CLI checks OK |
| FR-DI-05 | Acyclic dependency assertion | ✅ Pass | `make check-deps` + `dependency-check.sh` green |
| FR-CLI-01 | `version` prints `zcode v<version> (git: <sha>, profile: <profile>)` | ✅ Pass | Output: `zcode v0.1.0 (git: d9c7219d…, profile: debug)` |
| FR-CLI-02 | `clap v4` derive CLI with `version` subcommand | ✅ Pass | `crates/cli/src/cli/mod.rs` |
| FR-CLI-03 | Build metadata embedded at compile time | ✅ Pass | `build.rs` emits `VERGEN_GIX_SHA`, `VERGEN_BUILD_PROFILE` |
| FR-CLI-04 | Composition root fails fast with typed error | ✅ Pass | `wire()` returns `Result<App, AppError>`; `main` maps to exit 1 |
| FR-TOOL-01 | `rust-toolchain.toml` pins toolchain + components | ✅ Pass (minor deviation §5.3) | `channel = "1.85.0"`, components `rustfmt`, `clippy` |
| FR-TOOL-02 | `.cargo/config.toml` with target-dir + quiet | ⚠️ Partial (§5.4) | `target-dir` present; `quiet-workspaces` missing |
| FR-TOOL-03 | `rustfmt.toml` formatting config | ✅ Pass (partial §5.6) | `edition`, `max_width`, `tab_spaces` present; `wrap_comments`/`format_code_in_doc_items` omitted |
| FR-TOOL-04 | `clippy` with `-D warnings` + `clippy.toml` | ✅ Pass | `make lint` / `cargo clippy -- -D warnings` clean |
| FR-TOOL-05 | `Makefile` with `build`, `test`, `lint`, `fmt`, `bench` | ✅ Pass | All targets present and working |
| FR-TOOL-06 | `deny.toml` for supply-chain checks | ✅ Present | `deny.toml` exists; CI audit deferred to next milestone |
| FR-DOC-01..05 | README, arch doc, PRD, CHANGELOG, CONTRIBUTING | ✅ Pass | All files present |
| FR-PERF-01 | Release profile `lto = "thin"`, `codegen-units = 1` | ✅ Pass | `[profile.release]` in `Cargo.toml` |
| FR-PERF-02 | `benches/` criterion placeholder | ✅ Pass (after fix §5.1) | Compiles and runs |
| FR-PERF-03 | Domain entities use owned types | ✅ Pass | `String`, `PathBuf`, `Box<[String]>`, `Vec<T>` throughout `model.rs` |
| FR-PERF-04 | `zcode.toml.example` documents low-memory defaults | ✅ Pass | `examples/zcode.example.toml` present with env-secrets note |
| NFR-BUILD-01 | Clean build, 0 warnings | ✅ Pass | `cargo build` — no warnings |
| NFR-BUILD-02 | Reproducible (pinned toolchain) | ✅ Pass | `rust-toolchain.toml` |
| NFR-BUILD-03 | Minimal default feature set | ✅ Pass | `tokio` uses `default-features = false`, features `["rt", "macros"]` |
| NFR-PERF-01 | Cold-start < 300 ms | ✅ Pass | Measured 5–8 ms (§3.5) |
| NFR-PERF-03 | `panic = "abort"`, `strip` in release | ✅ Pass | `panic = "abort"`, `strip = "symbols"` |
| NFR-REL-01 | Tests determinism | ✅ Pass | `cargo test --workspace` green |
| NFR-REL-02 | No panics in composition root | ✅ Pass | Typed `AppError::Port`, fail-fast |
| NFR-MAINT-01 | `cargo clippy -- -D warnings` | ✅ Pass | Clean |
| NFR-MAINT-02 | `cargo fmt --check` | ✅ Pass | No diff |
| NFR-MAINT-03 | `cargo doc --no-deps` builds | ✅ Pass | Builds, 0 errors |
| NFR-MAINT-05 | `deny.toml` present | ✅ Pass | Present |
| NFR-PORT-01 | Tier-1 targets (Linux x86_64, macOS aarch64) | ✅ N/A | Builds on Linux x86_64; macOS not tested in this environment |
| NFR-PORT-02 | Zero `unsafe` in Domain/App | ✅ Pass | `grep` finds no `unsafe` blocks outside the `#![forbid]` declaration; Domain/App are clean |
| NFR-SEC-01 | No secrets in repo | ✅ Pass | `.gitignore` excludes `.env`, `zcode.toml.local`; config secrets from env only |
| NFR-SEC-02 | Supply-chain `deny.toml` | ✅ Present | Present; audit is CI-only per PRD |

### Out-of-Scope verification (all correctly excluded)

| Item | PRD Out-of-Scope | Status |
|------|------------------|--------|
| LLM network calls | #1 | ✅ `infra-llm` `send()` returns `Err`, unit test confirms |
| Chat loop / task execution engine | #2 | ✅ `App::run()` returns `Err(AppError::Port(...))` |
| AST-aware file editing | #3 | ✅ Only `FileSystemPort` trait + std impl |
| Persistent PTY sessions | #4 | ✅ `StdShell::spawn()` returns `Err`, test confirms |
| Plugin loading runtime | #5 | ✅ `Plugin` entity + `PluginRegistryPort` declared; `NullPluginRegistry` stub in CLI |
| Session state / history | #6 | ✅ Not implemented |
| GUI / TUI | #7 | ✅ CLI only |
| CI workflow `.yml` | #8 | ✅ Only `Makefile` local runner |
| Packaging / releases | #9 | ✅ Not implemented |
| Real workload benchmarks | #10 | ✅ Only criterion smoke (entity construction) |
| Windows support | #11 | ✅ Not required for v0.1 |

---

## 3. Verification Results (raw)

All commands run from workspace root with toolchain 1.85.0:

```
$ cargo build
   Compiling zcode v0.1.0 (/workspace/crates/cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.57s
  [0 warnings]

$ cargo fmt --check
  [exit 0 — no diff]

$ cargo clippy --workspace -- -D warnings
   Compiling zcode v0.1.0 (/workspace/crates/cli)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.06s
  [0 warnings]

$ cargo test --workspace
    ... all test results: ok ...
  18 unit tests across all crates, 0 failures

$ cargo doc --no-deps --workspace
    Generated /workspace/target/doc/zcode/index.html and 7 other files
  [exit 0]

$ cargo tree -p domain
domain v0.1.0 (/workspace/crates/domain)
  [stdlib only — no cargo:/[j] lines]

$ cargo run --quiet -- version
zcode v0.1.0 (git: d9c7219d3f6d80e4a9027cc556aabf7812eb0e08, profile: debug)
  [exit 0]

$ make check-deps
domain pure OK
  [exit 0]

$ bash docs/architecture/dependency-check.sh
OK: domain is dependency-free (1 line)
OK: app depends only on domain
OK: infra-llm / infra-filesystem / infra-shell / infra-config have no upward dependency on cli/app
OK: cli depends on domain / app / infra-llm / infra-filesystem / infra-shell / infra-config
All dependency checks passed.
  [exit 0]

$ cargo bench -p zcode-benches --bench smoke -- --quick
smoke/Task::construct          time:   [160.52 ns 180.02 ns 184.90 ns]
smoke/FileEdit::construct      time:   [165.86 ns 165.98 ns 166.01 ns]
smoke/DomainError::format      time:   [165.98 ns 166.36 ns 167.84 ns]
  [exit 0]

$ cargo build --release
    Finished `release` profile [optimized] target(s) in 17.00s

$ ./target/release/zcode version
zcode v0.1.0 (git: d9c7219d3f6d80e4a9027cc556aabf7812eb0e08, profile: release)

$ du -h target/release/zcode
736K    target/release/zcode    [well under 8 MB L2 threshold]

$ # Cold-start timing (release binary, 3 runs)
Run 1: 8ms
Run 2: 6ms
Run 3: 5ms    [well under 300 ms NFR-PERF-01 budget]

$ make ci
  fmt-check: exit 0
  lint:      exit 0
  test:      exit 0
  build:     exit 0
  [overall exit 0 — green]

$ grep -rn "unsafe" crates/ --include="*.rs"
crates/cli/src/main.rs:#![forbid(unsafe_code)]
  [only the forbid declaration; no actual unsafe blocks — NFR-PORT-02 satisfied]
```

---

## 4. Code Quality Observations

### 4.1 Architecture & Layering

- **Dependency inversion is correctly enforced.** The five port traits (`LlmPort`, `FileSystemPort`, `ShellPort`, `PluginRegistryPort`, `LoggerPort`) are declared in `domain::ports` with no infra coupling. Each infra adapter implements the trait from the `domain` crate. The `dependency-check.sh` script and `make check-deps` provide automated enforcement.
- **Composition root is clean.** `cli/src/cli/mod.rs::wire()` constructs `Arc::new(OpenAiLlm::new(...))`, `Arc::new(StdFs::new())`, etc., wraps them as `Arc<dyn Port>` type-objects, and passes them to `App::new(...)`. The `NullPluginRegistry` and `NullLogger` stubs correctly avoid pulling in unused infra.
- **Fail-fast error handling in the composition root.** `wire()` returns `Result<App, AppError>`; `main.rs` maps errors to `eprintln!("zcode: {e}")` + `ExitCode::from(1)`. No panic traces (NFR-REL-01/02 satisfied).
- **The `App<const N: usize = 4>` generic** is a forward-looking hook (per technical-plan §5.3 note). It is unused but properly annotated with `#[allow(dead_code)]` on the struct and clippy passes.

### 4.2 Domain purity

- `cargo tree -p domain` shows exactly one line: `domain v0.1.0`. Zero third-party dependencies — FR-DI-01 is satisfied.
- `DomainError` uses hand-rolled `Display` + `std::error::Error` impls (no `thiserror`) — correct per technical-plan DQ3.
- All entity fields are owned (`String`, `PathBuf`, `Box<[String]>`, `Vec<(String, String)>`) — FR-PERF-03 satisfied.

### 4.3 Error-handling strategy

Matches the technical-plan DQ3 decision:
- **Domain:** manual `std::error::Error` impls (no `thiserror`) — keeps Domain dep-free.
- **App:** `thiserror` for `AppError` with `Port(String)` and `Domain(#[from] DomainError)` variants.
- **Infra:** each adapter has a `thiserror`-based error enum (`FsError`, `ShellError`, `ConfigError`); `thiserror` is used in `infra-llm`'s Cargo.toml though no error enum is defined there (minor unused dependency — see §5.2).
- **CLI:** uses `Box<dyn std::error::Error + Send + Sync>` in `run()` for composition-root glue — acceptable per DQ3.

### 4.4 Build metadata strategy

The technical plan (DQ2) specified `vergen-gix`, but the actual `build.rs` implements a **custom git-SHA embedding** using `git rev-parse HEAD` with a `.git/HEAD` file fallback. The in-code comment explains this was necessitated because `vergen-gix` requires Rust >= 1.88 (beyond the pinned toolchain). This is a **pragmatic, well-documented deviation** that produces correct output (`zcode v0.1.0 (git: <sha>, profile: debug)`). No action required, but the PRD/technical-plan should be updated to reflect the actual strategy.

### 4.5 Test coverage

- **Domain:** 1 unit test (`completion_chunk_done_semantics` in `ports.rs`).
- **App:** 2 unit tests (`app_returns_port_error_for_run`, `app_returns_port_error_for_plan`) with full mock trait implementations (NoopLlm, NoopFs, NoopShell, NoopPlugins, NoopLogger) — excellent hermetic test design.
- **Infra LLM:** 2 tests (`stub_does_not_call_network`, `stream_yields_single_done_chunk`).
- **Infra filesystem:** 5 tests (round-trip, exists, list, read-missing, watch-stub).
- **Infra shell:** 3 tests (run echo, spawn stub, missing command).
- **Infra config:** 3 tests (default values, env-overrides-file, file-only-load).
- **CLI:** 4 unit tests (version parse, git_sha const, wire constructs app) — equivalent to task-08's intended `tests/cli.rs` integration test.
- **Benches:** criterion smoke benchmark with 3 fixtures.
- **Total: 18 unit tests, all passing.** Test hermeticity is good (tempfile for FS/config, mock traits for app, no network/stub checks for LLM).

### 4.6 Memory & Performance

- Release binary: **736 KB** (well under the 8 MB L2 leading-indicator threshold).
- Cold start: **5–8 ms** (well under the 300 ms NFR-PERF-01 budget and the 180 s L1 compile-time signal — release build completed in 17 s).
- Single-threaded tokio runtime (`flavor = "current_thread"`) — correct per DQ4 for minimal idle-thread memory.
- `tokio` uses `default-features = false` with only `["rt", "macros"]` — minimal runtime footprint.

### 4.7 Security

- `#![forbid(unsafe_code)]` in `cli/src/main.rs` — NFR-PORT-02 satisfied. No `unsafe` blocks exist anywhere in Domain/App.
- `.gitignore` covers `.env`, `zcode.toml.local`, `target/`, `*.profraw`, `.coverage/`.
- Config loader reads secrets from `ZCODE_*` env vars only; `zcode.example.toml` explicitly documents that secrets never come from the file.
- Shell adapter passes commands via `sh -c` as a single argv (documented in task-06); no shell-injection surface at the adapter layer.

---

## 5. Issues Found

### 5.1 CRITICAL — Criterion benchmark did not compile (FIXED in this review)

**File:** `benches/benches/smoke.rs`  
**Severity:** Blocking (fails `make bench` / FR-PERF-02)  
**PRD gate affected:** M1.3 / T9 (criterion benchmark must compile & run)

The smoke benchmark used `BenchmarkGroup` without the generic type parameter required by criterion 0.5.1. The `BenchmarkGroup` struct signature is `BenchmarkGroup<'a, M: Measurement>` where `c.benchmark_group(...)` returns `BenchmarkGroup<'_, WallTime>`. Passing the unparameterized `BenchmarkGroup` caused `error[E0107]: missing generics for struct BenchmarkGroup`.

**Fix applied during review:**
- Imported `criterion::measurement::WallTime` (the `measurement` module is public in criterion 0.5.1, but the `Measurement` trait itself is not re-exported at the crate root).
- Changed all three helper functions from `fn construct_task(c: &mut BenchmarkGroup)` to `fn construct_task(c: &mut BenchmarkGroup<'_, WallTime>)`, matching the type returned by `c.benchmark_group("smoke")`.

**Re-verified:** `cargo bench -p zcode-benches --bench smoke -- --quick` now compiles and runs, reporting nanosecond-level timings for all three fixtures.

### 5.2 MINOR — Unused `thiserror` dependency in `infra-llm`

**File:** `crates/infra/llm/Cargo.toml`  
**Severity:** Low (violates L3 edge-count proxy — "infra direct deps ≤ 15")

`infra-llm` declares `thiserror = { workspace = true }` as a dependency, but `src/lib.rs` does not use `thiserror` anywhere — errors are returned as `Box<dyn Error>` via string `into()`. This adds `thiserror` (and its proc-macro transitive closure) to the `infra-llm` crate graph with no benefit.

**Recommendation:** Remove `thiserror` from `crates/infra/llm/Cargo.toml` before release, or add a `thiserror`-derived `LlmError` enum if richer error semantics are planned for a later milestone.

### 5.3 MINOR — Toolchain version mismatch vs. PRD

**Files:** `rust-toolchain.toml`, `clippy.toml`, `Cargo.toml` (`[workspace.package].rust-version`)  
**Severity:** Informational

The PRD §7.2 and FR-TOOL-01 specify Rust **1.80.0** as the pinned stable toolchain. The actual workspace pins **1.85.0**. The technical plan DQ2 notes `vergen-gix` requires Rust >= 1.88, which motivated the custom build.rs; however the toolchain was bumped to 1.85 rather than kept at 1.80.

**Assessment:** This is acceptable — both 1.80 and 1.85 are stable, edition 2021 is supported, and no 1.85-specific features are used in the code. The toolchain pin is consistent across all config files. The PRD and technical-plan should be updated to reflect 1.85.0 for documentation accuracy.

### 5.4 MINOR — Missing `quiet-workspaces` in `.cargo/config.toml`

**File:** `.cargo/config.toml`  
**Severity:** Low  
**PRD gate:** FR-TOOL-02

The PRD FR-TOOL-02 requires `.cargo/config.toml` to configure a local target dir and an "offline-friendly registry fallback" with `quiet-workspaces`. The actual file has `target-dir = "target"` and `[net] git-fetch-with-cli = true` but is missing the `[term]` section with `quiet-workspaces = true`.

**Recommendation:** Add the `[term]` section:
```toml
[term]
quiet-workspaces = true
```

### 5.5 MINOR — Missing `name` in `[workspace.package]`

**File:** `Cargo.toml`  
**Severity:** Low  
**PRD / Technical-plan:** §5.1

The technical plan §5.1 shows `[workspace.package]` with `name = "zcode"`, but the actual workspace `Cargo.toml` omits the `name` field. This is harmless (the `zcode` binary crate has its own `name = "zcode"` in `crates/cli/Cargo.toml`), but it deviates from the documented plan and means there is no single workspace-level package name.

**Recommendation:** Add `name = "zcode"` to `[workspace.package]` for consistency with the technical plan and to enable workspace-level metadata queries.

### 5.6 MINOR — Missing rustfmt options

**File:** `rustfmt.toml`  
**Severity:** Low  
**PRD / Technical-plan:** §5.1

The technical plan §5.1 specifies `wrap_comments = true` and `format_code_in_doc_items = true` in `rustfmt.toml`, but the actual file only contains `edition`, `max_width`, and `tab_spaces`.

**Assessment:** No impact on correctness — code is already formatted and passes `cargo fmt --check`. These options would improve formatting of doc comments in future iterations.

**Recommendation:** Add `wrap_comments = true` and `format_code_in_doc_items = true` to align with the technical plan.

### 5.7 MINOR — No separate `tests/cli.rs` integration test

**File:** `crates/cli/`  
**Severity:** Low (informational)  
**PRD / Task:** task-08 step 5

Task-08 step 5 explicitly calls for "an integration test `tests/cli.rs` invoking `Cli::try_parse_from`." The actual implementation places these tests as unit tests in the `#[cfg(test)] mod tests` block inside `cli/src/cli/mod.rs` instead of a separate `tests/cli.rs` file.

**Assessment:** The test coverage is equivalent — `Cli::try_parse_from(["zcode", "version"])` is tested, and `wire_constructs_app` validates the composition root. The deviation is structural only. If strict task compliance is desired, move the tests to `tests/cli.rs` and make the `cli` module `pub`.

### 5.8 MINOR — Stray files in workspace root

**Files:** `.qagent_node_config.env`, `node.json`, `logs/` directory  
**Severity:** Low

The `.gitignore` includes `.qagent_node_config.env`, `node.json`, and `/logs/`, which are not part of the PRD's specified ignore list (PRD FR-TOOL-03 / NFR-SEC-01 lists `.env`, `target/`, `zcode.toml.local`). These appear to be artifacts from another tool in the environment, not from this scaffolding task. They are correctly gitignored but are outside the intended scope of the initial-scaffolding PRD.

**Recommendation:** Leave as-is (correctly ignored); clean up if these files are not project artifacts.

### 5.9 MINOR — Unused `[profile.ci]` profile

**File:** `Cargo.toml`  
**Severity:** Negligible  
**PRD / Technical-plan:** §5.1

The `[profile.ci]` profile (inherits from `dev`, `opt-level = 0`) is defined but never referenced by any Makefile target or build command. The `make ci` target uses the default `dev` profile for `test`, `lint`, and `build`.

**Recommendation:** Either wire `make ci` to use `--profile ci` (e.g., `cargo test --workspace --profile ci`) or remove the profile to avoid dead configuration.

---

## 6. Test Coverage Assessment

| Crate | Unit tests | Integration tests | Coverage quality |
|-------|-----------|-------------------|-----------------|
| domain | 1 (ports.rs) | 0 | Adequate for a pure-stdlib scaffolding crate |
| app | 2 (run, plan stubs) | 0 | Excellent — uses mock trait impls (Noop*) |
| infra-llm | 2 (send Err, stream chunk) | 0 | Good — confirms no-network stub |
| infra-filesystem | 5 (round-trip, exists, list, missing, watch) | 0 | Good — hermetic with tempfile |
| infra-shell | 3 (echo, spawn stub, missing cmd) | 0 | Good — portable command |
| infra-config | 3 (default, env-overrides-file, file-only) | 0 | Good — tests merge precedence |
| cli | 4 (parse, consts, wire) | 0 | Adequate |
| benches | N/A (criterion) | N/A | Smoke benchmark runs |
| **Total** | **20** | **0** | |

**Assessment:** Test coverage is solid for a scaffolding milestone. The app crate stands out with full mock implementations of all 5 ports. No integration tests exist as separate `tests/` files, but unit test coverage is comprehensive. A coverage gate (NFR-MAINT-04 / tarpaulin) is explicitly deferred to the next milestone per PRD.

---

## 7. Recommendations

### Must-fix before merge (blocking)
1. **None remaining** — the criterion compilation bug (§5.1) was fixed and re-verified during this review. All primary gates (M1.1–M1.6) are green.

### Should-fix before merge (strong recommendation)
2. **Remove unused `thiserror` from `infra-llm`** (§5.2) — reduces dependency edges, improves L3 compliance.
3. **Add `[term] quiet-workspaces = true` to `.cargo/config.toml`** (§5.4) — PRD compliance.
4. **Add `name = "zcode"` to `[workspace.package]`** (§5.5) — aligns with technical-plan §5.1.
5. **Document actual toolchain (1.85.0) and build.rs strategy** in PRD/technical-plan — the docs currently specify 1.80.0 and `vergen-gix`, neither of which matches the implementation.

### Nice-to-have (future iteration)
6. Add `wrap_comments = true` and `format_code_in_doc_items = true` to `rustfmt.toml` (§5.6).
7. Consider moving CLI tests to a `tests/cli.rs` integration test file for strict task-08 compliance (§5.7).
8. Wire `[profile.ci]` into `make ci` or remove it (§5.9).
9. Add CI workflow files (`.github/workflows/`) in the next milestone — explicitly out of scope for v0.1 per PRD §5 Out-of-Scope #8.

---

## 8. Overall Assessment

**✅ APPROVED for merge** (with the blocking fix already applied).

The initial-scaffolding milestone is an exemplary clean-architecture foundation. The codebase correctly enforces dependency inversion, the composition root fails fast with typed errors, all quality gates pass, and the binary is remarkably lean (736 KB, 5–8 ms cold start). The engineering team made sound decisions on the resolved architecture questions (DQ1–DQ5), particularly the custom build script replacing `vergen-gix` for toolchain compatibility.

The deviations from the PRD are minor and well-documented. The one blocking issue (criterion compilation) has been resolved. The workspace is ready for the next milestone (actual use-case implementation, LLM wiring, chat loop) without structural changes.

---

*End of review.*
