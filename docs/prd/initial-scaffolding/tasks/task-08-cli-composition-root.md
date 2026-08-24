# Task 08 — CLI Composition Root + Version Subcommand

**Related PRD sections:** §3.3 CLI, §3.4 Dependency Flow, FR-CLI-01..04, FR-DI-04/05, DQ2 (vergen gix), DQ4 (tokio current-thread)
**Depends on:** task-02..07 (Domain, App, all infra crates)
**Status:** To do

## Objective
Build `crates/cli` (`ag`) with `clap v4`, a `version` subcommand printing `ag v<version> (git: <sha>, profile: <profile>)`, build-metadata via `vergen-gix`, a `#[tokio::main(flavor="current_thread")]` entry, and the **composition root** `wire()` that constructs concrete infra-backed `App`s (stubbed use-cases) and fails fast with a typed error on unresolved ports.

## Step-by-step

1. Create `crates/cli/Cargo.toml` — deps on `domain`, `app`, all `infra/*`, `clap`, `tokio` (`rt` only), `log`, `env_logger`; `build-dependencies` = `vergen-gix` + `vergen-config`.
2. Create `crates/cli/build.rs` emitting `VERGEN_GIX_SHA`, `VERGEN_BUILD_PROFILE` via `vergen_gix::gix::add_instructions` with a safe fallback to `"unknown"`.
3. Create `crates/cli/src/main.rs` — `#![forbid(unsafe_code)]`, `#[tokio::main(flavor="current_thread")]`, calls `ag::cli::run()`, maps error to `eprintln!("ag: {e}")` + `ExitCode::from(1)`.
4. Create `crates/cli/src/lib.rs` exposing `pub mod cli;`.
5. Create `crates/cli/src/cli/mod.rs`:
   - `Cli` derive (`#[command(name="ag", version, about)]`) + `Commands` enum (`Version`).
   - compile-time consts `VERSION`, `GIT_SHA`, `BUILD_PROFILE` via `env!`.
   - `run()` matches `Version` → println the required string.
   - `wire()` helper constructing `App::new(...)` from concrete adapters (documented, used by future commands); returns `Result<(), AppError>`.
6. Add an integration test `tests/cli.rs` invoking `Cli::try_parse_from` and asserting `Version` parses.

## Test-case scenario
- `cargo run --quiet -- version` prints exactly `ag v0.1.0 (git: <sha>, profile: <profile>)`; an unknown subcommand yields a clap error (exit 2), not a panic.

## How to verify
```
cargo test -p ag
cargo run --quiet -- version          # prints version line (T8)
cargo build --release                 # T1
cargo clippy -p ag -- -D warnings
size -A target/release/ag             # L2 < 8MB proxy
```
**Pass criteria:** version output matches the format; `cargo tree -p ag` lists all layers; no `unsafe` in `cli` (NFR-PORT-02).

## Success metric mapping
- M1.1 (release build), M1.6 (version), NFR-PERF-01 (current-thread), NFR-PORT-02, M1.4 (composition root = sole cycle target).
