# Task 01 — Workspace & Tooling Configuration

**Related PRD sections:** FR-TOOL-01..06, NFR-BUILD-01..03, FR-PERF-01/03
**Depends on:** none (foundation task)
**Status:** To do

## Objective
Establish the Cargo workspace, pin the toolchain, and configure all static quality tooling so every subsequent crate inherits consistent builds, formatting, and lint rules.

## Step-by-step

1. Create `rust-toolchain.toml` pinning stable **1.80.0** with components `rustfmt`, `clippy`.
2. Create `.cargo/config.toml` setting `target-dir = "target"` and `quiet-workspaces`.
3. Create the workspace root `Cargo.toml` (members listed in technical-plan §5.1; release profile `lto="thin"`, `codegen-units=1`, `panic="abort"`, `strip="symbol"`; `rt` tokio feature).
4. Create `rustfmt.toml` (edition 2021, max_width 100, tab_spaces 4, wrap_comments).
5. Create `clippy.toml` (msrv 1.80.0).
6. Create `deny.toml` (advisories, licenses: allow OSI/FSF free, deny copyleft).
7. Create `.gitignore` (`./target`, `.env`, `ag.toml.local`, `*.profraw`, `.coverage/`).

## File tree created
```
Cargo.toml
rust-toolchain.toml
.cargo/config.toml
rustfmt.toml
clippy.toml
deny.toml
.gitignore
```

## Test-case scenario
- A fresh clone running exactly two commands yields a green build.

## How to verify
```
rustup show                          # toolchain == 1.80.0, components present
cargo metadata --no-deps --format-version 1 | grep '"name":"ag"'   # workspace root resolves
cargo fmt --check                    # no diff (T4)
cargo build                          # compiles, 0 warnings (T1)
cat target/.rustc_info.json &>/dev/null; true   # sanity that target is writable
```
**Pass criteria:** `rustup show` reports `1.80.0` with `clippy`/`rustfmt`; `cargo fmt --check` exits 0; `cargo build` exits 0 with zero warnings.

## Success metric mapping
- M1.1 (build green), M1.3 (fmt/clippy), NFR-BUILD-01/02, L1 compile time signal.
