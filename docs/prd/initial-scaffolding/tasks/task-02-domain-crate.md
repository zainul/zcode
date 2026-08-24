# Task 02 — Domain Crate (entities, error, ports)

**Related PRD sections:** §3.1 Domain, §3.2 Entities, FR-DI-01, G3/G4, NFR-PORT-02
**Depends on:** task-01
**Status:** To do

## Objective
Produce a dependency-free Domain crate (`crates/domain`) holding the core vocabulary (entities), a pure-stdlib `DomainError`, and the five port traits that embody the dependency-inversion boundary. This is the root of the dependency graph and must compile with **zero** third-party crates.

## Step-by-step

1. Create `crates/domain/Cargo.toml` — `name="domain"`, workspace-inherited version/edition; **no `[dependencies]` section** (stdlib only).
2. Create `crates/domain/src/lib.rs` — re-export submodules `error`, `model`, `ports`.
3. Create `crates/domain/src/model.rs` — owned entities (`Task`, `TaskStatus`, `FileEdit`, `ShellCommand`, `Plugin`, `AgentContext`) using `String`/`PathBuf`/`Box<[String]>`.
4. Create `crates/domain/src/error.rs` — `DomainError` enum with hand-rolled `Display` + `std::error::Error` impls (no `thiserror`).
5. Create `crates/domain/src/ports.rs` — `LlmPort`, `FileSystemPort`, `ShellPort`, `PluginRegistryPort`, `LoggerPort` traits + `CompletionChunk` struct + `LogLevel` enum.
6. Add a unit test in `ports.rs` that constructs a trivial `CompletionChunk` and asserts `done` semantics.

## Test-case scenario
- Domain is the dependency root; any third-party transitive dep is a regression.

## How to verify
```
cargo test -p domain                     # passes (T2)
cargo tree -p domain                     # lists ONLY std (/rust). No cargo: lines (T6)
cargo clippy -p domain -- -D warnings    # clean
cargo doc -p domain --no-deps            # builds
```
**Pass criteria:** `cargo tree -p domain` shows no `[j`-prefixed or `cargo:` lines (M1.5); `cargo test -p domain` green (M1.2); `DomainError` is `Display`+`Error` and `#[derive(Debug)]`.

## Success metric mapping
- M1.2, M1.5, M1.3, NFR-PORT-02 (zero unsafe in Domain).
