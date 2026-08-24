# Task 03 — Application Crate (orchestrator + use-case traits)

**Related PRD sections:** §3.1 App, §3.2 TaskRunner/EditPlanner, FR-DI-02, G3
**Depends on:** task-02 (Domain)
**Status:** To do

## Objective
Create `crates/app`, the use-case orchestration layer. It depends **only** on `domain` and declares the `TaskRunner` / `EditPlanner` contracts plus an `App` orchestrator parameterized by boxed port trait-objects. Behavior returns a typed `AppError::Port` stub (fail-fast, no panic) — the chat loop ships in a later milestone.

## Step-by-step

1. Create `crates/app/Cargo.toml` — dep on `domain` (path) + `thiserror` (workspace).
2. Create `crates/app/src/lib.rs` exporting `AppError`, `TaskRunner`, `EditPlanner`, `App`.
3. Define `AppError` (`thiserror::Error`) with `Port(String)` + `Domain(#[from] DomainError)` variants.
4. Define `TaskRunner` and `EditPlanner` traits taking `&AgentContext`.
5. Implement both traits on `App` returning `Err(AppError::Port(...))` with a clear "not implemented in v0.1.0" message (NFR-REL-01).
6. Add `#[derive(Debug)]` and `#[allow(dead_code)]` on `App` fields where unused.

## Test-case scenario
- App must never reference `infra/*` or `cli`; the orchestrator must fail fast with a typed error, not a panic.

## How to verify
```
cargo test -p app
cargo tree -p app | grep -E 'domain|thiserror'   # only domain + thiserror (no infra/cli)
cargo clippy -p app -- -D warnings
```
A negative check: `cargo tree -p app` must **not** list `infra-llm`, `infra-filesystem`, etc.

**Pass criteria:** `cargo tree -p app` references only `domain` and `thiserror` (FR-DI-02); `cargo test -p app` green; `App::run` returns `Err(AppError::Port(...))`.

## Success metric mapping
- M1.2, M1.3, NFR-REL-01/02.
