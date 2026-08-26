# Task 04 — Infra: LLM Adapter

**Related PRD sections:** §3.1 infra, §3.2 LlmPort, Out-of-Scope #1 (no network calls)
**Depends on:** task-02 (Domain)
**Status:** Done

## Objective
Implement the OpenAI-compatible `LlmPort` in `crates/infra/llm`. Deliver the trait impl shape **only** — no API key field wired to a network request in v0.1.0 (per Out of Scope #1). A unit test asserts the stub never reaches the network.

## Step-by-step

1. Create `crates/infra/llm/Cargo.toml` — dep on `domain` (path) + `thiserror` (workspace).
2. Create `crates/infra/llm/src/lib.rs` exposing `OpenAiLlm` struct (`endpoint: String`, `model: String`).
3. Implement `domain::LlmPort` for `OpenAiLlm`:
   - `send` → `Err("llm network disabled in v0.1.0")`.
   - `stream` → `Box` over a single `CompletionChunk { delta: "", done: true }`.
4. Add a unit test asserting `send()` returns `Err` and `stream()` yields exactly one `done` chunk.

## Test-case scenario
- The adapter compiles and the port is wired, but no HTTP request is ever issued.

## How to verify
```
cargo test -p infra-llm
cargo clippy -p infra-llm -- -D warnings
```
**Pass criteria:** `cargo test -p infra-llm` green; `send()` returns `Err`; no `reqwest`/`hyper` dependency present (`cargo tree -p infra-llm`).

## Success metric mapping
- M1.2, M1.3, Out-of-Scope guard, L3 edge-count proxy (infra direct deps ≤ 15).
