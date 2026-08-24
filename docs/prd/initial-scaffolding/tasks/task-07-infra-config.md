# Task 07 — Infra: Configuration Model & Loader

**Related PRD sections:** §3.1 infra, §3.2 AgentContext, FR-PERF-04 (low-memory defaults), NFR-SEC-01
**Depends on:** task-02 (Domain)
**Status:** To do

## Objective
Create `crates/infra/config` with a serde `Config` model and a `TomlConfigLoader` that merges `std::env` (AG_ prefixed) and `ag.toml`. Secrets are read from env only, never written to disk. `ag.toml.local` is gitignored.

## Step-by-step

1. Create `crates/infra/config/Cargo.toml` — dep on `domain` + `serde`, `toml`; no other net crates.
2. Create `crates/infra/config/src/lib.rs` exposing `Config` struct (`model: String`, `working_dir: PathBuf`, `env: Vec<(String,String)>`, `timeout_ms: u64`) and `Loader`.
3. `Loader::load()` reads `ag.toml` if present (deserialize), then overlays `AG_*` env vars.
4. Provide `Config::default()` tuned for low memory (FR-PERF-04): small `timeout_ms`, conservative default model name, `working_dir = current dir`.
5. Add `examples/ag.example.toml` documenting defaults + that secrets come from env.
6. Add a unit test using a tempdir: write `ag.toml`, load, assert merged values.

## Test-case scenario
- Loading merges `ag.toml` + `AG_MODEL=gpt-4o` with env overriding file.

## How to verify
```
cargo test -p infra-config
cargo clippy -p infra-config -- -D warnings
cargo doc -p infra-config --no-deps
```
**Pass criteria:** override precedence (env > file) holds in test; `ag.toml.local` listed in `.gitignore` (task-01); `Config::default()` sets conservative timeout.

## Success metric mapping
- M1.2, M1.3, FR-PERF-04, NFR-SEC-01, L3 edge count (serde/toml only).
