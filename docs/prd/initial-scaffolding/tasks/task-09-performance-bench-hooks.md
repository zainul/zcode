# Task 09 — Performance & Bench Hooks

**Related PRD sections:** FR-PERF-01/02/03, NFR-PERF-01/02/03, L1/L2 edge-counts
**Depends on:** task-02 (Domain) for bench fixture; independent otherwise
**Status:** Done

## Objective
Wire the memory/performance mandate into the scaffolding: a criterion smoke benchmark, release profile tuning, and a binary-size/cold-start baseline script. This sets up regression tracking for future milestones.

## Step-by-step

1. Create `benches/Cargo.toml` — `zcode-benches` crate, dep on `domain` + `criterion`; `[[bench]] name="smoke" harness=false`; `[lib] bench=false`.
2. Create `benches/benches/smoke.rs` — criterion `BenchmarkGroup` measuring `Task` + `FileEdit` construction and `DomainError` round-trip (pure-domain fixture).
3. Add a `make size` target printing `target/release/zcode` bytes (task-11).
4. Confirm `Cargo.toml` release profile already carries `lto="thin"`, `codegen-units=1`, `panic="abort"`, `strip="symbol"` (set in task-01).

## Test-case scenario
- `cargo bench` runs the smoke benchmark without panicking; criterion emits `new/estimate` timing.

## How to verify
```
cargo bench -p zcode-benches --bench smoke -- --quick
ls -la target/release/zcode               # L2 < 8MB
```
**Pass criteria:** `cargo bench` compiles & runs; smoke benchmark reports a median; release binary size recorded (< 8 MB target L2).

## Success metric mapping
- L1 (compile-time signal), L2 (binary size), FR-PERF-01/02/03.
