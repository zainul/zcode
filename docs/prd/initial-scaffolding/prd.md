# PRD: Initial Project Scaffolding — Clean Architecture in Rust

**Document ID:** PRD-SCAFFOLD-001
**Status:** Draft
**Author:** Technical Product Manager
**Created:** 2026-08-24
**Target Release:** v0.1.0 (Foundation milestone)
**Owner:** Engineering Team

---

## 1. Overview and Goals

### 1.1 Overview

The AI Coding Agent (`ag`) is a terminal-based, AI-driven coding assistant written in Rust that mirrors **1:1 the core capabilities of OpenCode** while deliberately reducing memory footprint and maximizing runtime performance. This document defines the requirements for the **initial project scaffolding** that establishes the clean, layered codebase upon which all subsequent features will be built.

This initial milestone is **non-negotiable on structure**: it does not deliver user-facing agent behavior, but it creates the durable, testable, and maintainable foundation that makes future feature velocity possible. The scaffolding must make the **right engineering tradeoffs** explicit at day one so that every future feature inherits low coupling, clear boundaries, and a minimal memory profile.

### 1.2 Goals (Why we are doing this)

| # | Goal | Rationale |
|---|------|-----------|
| G1 | Establish a Rust workspace using **Clean Architecture** (Domain → Application → Infrastructure → Interface) so ownership boundaries prevent feature bleed. | Prevents the spaghetti layer cake that killed many prior agent codebases; critical for a multi-month roadmap. |
| G2 | Produce a compilable baseline (`cargo build`) and a passing `cargo test` baseline before any business logic ships. | "It builds, it tests" is the contract every contributor inherits; avoids the broken-main-branch death spiral. |
| G3 | Define the crate topology and dependency flow rules that enforce **dependency inversion** (inner layers define traits, outer layers implement them). | Guarantees the Domain is pure and framework-agnostic; enables fast, deterministic unit tests. |
| G4 | Encode the **memory-efficiency** mandate into the scaffolding: default to `Box<[T]>`, arena-free designs, and avoid runtime GC or heavy reflection. | Memory efficiency is a competitive differentiator vs. OpenCode's JS baseline; must be the default, not a refactor. |
| G5 | Provide a reproducible **tooling chain** (lint, format, typecheck-equivalent) so code quality gates are automatic from the first PR. | Catches issues before humans review; enforces consistency across a distributed team. |
| G6 | Ship a **minimal runnable CLI binary** that resolves to a `version` subcommand and prints build metadata (version, git SHA, build profile). | Proves the build pipeline end-to-end and gives early contributors a success signal. |
| G7 | Deliver a living **backlog seed** of domain entities and use-case interfaces so the roadmap has a shared vocabulary. | Translates the OpenCode feature set into actionable, estimable work for later sprints. |

### 1.3 Vision Alignment

This scaffolding directly serves the overarching product vision:

> **Build a terminal coding agent in Rust, 1:1 capable of OpenCode's core features (natural language task execution, file editing, shell execution, plugin support), while being significantly leaner on memory and faster on cold start.**

Success at this milestone is measured not by lines of agent logic shipped, but by the **quality and testability of the foundation**. A poorly structured base will compound into massive rearchitecture debt; a clean base accelerates every future feature.

---

## 2. User Stories

> In clean architecture, "users" at this milestone are primarily **contributors and maintainers**. End-user stories are deferred to later milestones but included here as context for the entities/use-cases the scaffolding must *enable*.

### 2.1 Contributor-Facing Stories (MVP scope)

| ID | As a… | I want to… | So that… |
|----|--------|------------|----------|
| US-C-01 | Contributor | generate the project from a documented bootstrap command (`cargo build` works on a fresh clone) | I can validate my environment without hunting for tooling. |
| US-C-02 | Contributor | run `cargo test` and see a green, deterministic suite | I know my changes did not break invariants. |
| US-C-03 | New contributor | see a `README` with architecture diagram + crate map and layer rules | I understand where to put new code without asking. |
| US-C-04 | Engineer | add a new layer (e.g., a new `Interface adapter`) without editing core crates | the architecture enforces boundaries and I do not violate dependencies by accident. |
| US-C-05 | Maintainer | run `cargo clippy`/`cargo fmt` and have CI enforce them | style and lint drift do not accumulate. |
| US-C-06 | Engineer | have domain logic testable in isolation (no I/O, no network) | tests are fast and hermetic, not flaky. |

### 2.2 End-User Stories (enabled, not delivered)

| ID | As a… | I want to… | So that… |
|----|--------|------------|----------|
| US-E-01 | User | ask the agent a natural-language task ("refactor this function") | the agent executes it via file/shell actions. |
| US-E-02 | User | have the agent edit/create/delete files in a project tree | I can delegate repetitive code edits. |
| US-E-03 | User | have the agent run shell commands on my behalf | I can script multi-step workflows. |
| US-E-04 | User | install/load a plugin to extend behavior | the agent adapts to my workflow. |
| US-E-05 | User | observe a small memory footprint even on large projects | the agent doesn't bog down my machine. |

> **Note:** US-E-* stories are *in scope as architectural constraints*, meaning the scaffolding must **not close the door** on them. They are delivered in subsequent milestones.

---

## 3. Functional Requirements

### 3.1 Workspace Topology (Clean Architecture Layers)

The project is structured as a Cargo **workspace** with explicit crates per architectural layer. Dependency direction is enforced: `Interface → Infrastructure → Application → Domain`. Each layer speaks to the next only through **traits defined in the inner layer** (dependency inversion).

| Layer | Crate(s) | Responsibility | Allowed dependencies |
|-------|----------|----------------|----------------------|
| **Domain** | `crates/domain` | Core business rules, entities, domain errors, domain traits (abstractions). No I/O. | None (stdlib only). |
| **Application** | `crates/app` | Use cases / orchestrators. Coordinates domain logic with ports. | Depends on `domain` only. |
| **Infrastructure** | `crates/infra/*` | Concrete adapters: LLM API client, filesystem, shell, plugins, config. | Depends on `domain` (for traits it implements) and external crates. |
| **Interface** | `crates/cli` | CLI parsing, user interaction loop, wiring/composition root. | Depends on all layers (composition root). |

**Concrete crates to be created in this milestone:**

- `crates/domain` — `Cargo.toml` + `lib.rs` (placeholder entity + domain error).
- `crates/app` — `Cargo.toml` + `lib.rs` (placeholder use-case trait).
- `crates/infra/llm` — LLM provider port + OpenAI-compatible adapter (trait impl only, no secrets).
- `crates/infra/filesystem` — Filesystem operations trait + std-backed impl.
- `crates/infra/shell` — Shell execution trait + std::process-backed impl.
- `crates/infra/config` — Configuration model + loader (env + `ag.toml`).
- `crates/cli` — `main.rs` + `cli/` module with `version` subcommand + wire-up (DI composition root) + `README` snippet.

### 3.2 Domain Entities (Scaffolded)

The Domain crate must define the core vocabulary as `struct`s/enum`s` (not yet behavior-rich) so future features have typed targets:

| Entity | Fields (scaffolded) | Owner |
|--------|---------------------|-------|
| `Task` | id, description, status, constraints | App |
| `FileEdit` | path, old_content, new_content | App |
| `ShellCommand` | command, cwd, env, timeout_ms | App |
| `Plugin` | name, version, manifest_path, entrypoint | Infra |
| `AgentContext` | working_dir, model, env | App |

Domain traits (ports) to be declared now:

- `trait LlmPort` (send messages, stream completions)
- `trait FileSystemPort` (read/write/list/watch)
- `trait ShellPort` (spawn, stream output)
- `trait PluginRegistryPort` (discover, load, execute)
- `trait LoggerPort` (structured logging interface)

> Each trait is declared in Domain; concrete impls live in `infra/*`. This is the dependency-inversion boundary that must be baked in now.

### 3.3 CLI Interface

| Requirement | Description |
|-------------|-------------|
| FR-CLI-01 | `cargo run --quiet -- version` prints `ag v<version> (git: <sha>, profile: <profile>)`. |
| FR-CLI-02 | CLI is built with `clap v4` (derive) with a top-level `ag` command and subcommands `version`. |
| FR-CLI-03 | Build metadata (version from `CARGO_PKG_VERSION`, git SHA from `vergen`/`git` env, profile) is embedded at compile time. |
| FR-CLI-04 | Composition root wires Domain/App/Infra and panics with a clean message on unresolved dependencies. |

### 3.4 Dependency Flow Enforcement

| Requirement | Description |
|-------------|-------------|
| FR-DI-01 | `domain` crate has **zero** third-party dependencies (stdlib only). Verified by `cargo tree -p domain` and CI gate. |
| FR-DI-02 | `app` crate depends only on `domain`; it references no concrete infra crates directly. |
| FR-DI-03 | Each `infra` crate depends on `domain` (to implement traits) plus external crates; it must not depend on `app` or `cli`. |
| FR-DI-04 | `cli` crate holds the only valid reference cycle target (composition root) and depends on all layers. |
| FR-DI-05 | A CI check (or a `deny` rule / `cargo-depgraph` script) asserts the above directional acyclicity. |

### 3.5 Tooling & Quality Gates

| Requirement | Description |
|-------------|-------------|
| FR-TOOL-01 | `rust-toolchain.toml` pins a stable Rust toolchain (e.g., `1.80` or latest stable) with components `rustfmt`, `clippy`. |
| FR-TOOL-02 | `.cargo/config.toml` configures a local target dir and offline-friendly registry fallback. |
| FR-TOOL-03 | `rustfmt.toml` enforces formatting consistent with project style (tab width, max width). |
| FR-TOOL-04 | `clippy` runs with `-D warnings` and a baseline `clippy.toml`. |
| FR-TOOL-05 | A `Makefile` (or `justfile`) exposes: `make build`, `make test`, `make lint`, `make fmt`, `make bench` (stubbed). |
| FR-TOOL-06 | A `deny.toml` is present for cargo-audit-friendly supply chain checks (even if advisory DB check is CI-only). |

### 3.6 Project Documentation Scaffold

| Requirement | Description |
|-------------|-------------|
| FR-DOC-01 | `README.md` explains the architecture, layer rules, how to build/test, and the contributor quick-start. |
| FR-DOC-02 | `docs/architecture/README.md` contains an ASCII or Mermaid dependency-flow diagram of the crate graph. |
| FR-DOC-03 | `docs/prd/initial-scaffolding/prd.md` is this document. |
| FR-DOC-04 | A `CHANGELOG.md` stub following "Keep a Changelog" conventions. |
| FR-DOC-05 | `CONTRIBUTING.md` stub with the build/test/lint commands. |

### 3.7 Memory & Performance Baseline Hooks

The scaffolding **establishes the hooks** for the memory-efficiency mandate but does not yet implement heavy logic:

| Requirement | Description |
|-------------|-------------|
| FR-PERF-01 | `Cargo.toml` for the workspace sets `lto = "thin"` and `codegen-units = 1` for release builds (binary size + perf). |
| FR-PERF-02 | A `benches/` directory exists with a criterion placeholder benchmark crate (stub) so perf regression tracking is wired early. |
| FR-PERF-03 | Domain entities use owned `String`/`Box<[T]>` types over `&str`/references where it avoids lifetimes in core paths; this choice is documented in code comments. |
| FR-PERF-04 | `ag.toml` example explicitly documents that defaults are tuned for low memory (model context window sizing guidance TBD). |

---

## 4. Non-Functional Requirements

### 4.1 Build & Compilation

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-BUILD-01 | Clean build from fresh clone | `cargo build` succeeds with no warnings in default profile within 120s on a standard CI runner. |
| NFR-BUILD-02 | Reproducible | `rust-toolchain.toml` pins toolchain; builds are deterministic across machines. |
| NFR-BUILD-03 | Minimal default feature set | No optional dependencies enabled by default that bloat compile time or binary. |

### 4.2 Performance & Memory

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-PERF-01 | Cold-start budget | `cargo run --release -- version` returns in < 300ms on a 2023 laptop. *(Measured via harness in later milestone; scaffolding provides the binary to measure.)* |
| NFR-PERF-02 | Memory ceiling | Domain/Infra layers avoid heap allocations in hot loops by design (documented ownership rules). |
| NFR-PERF-03 | Release profile optimization | `lto = thin`, `codegen-units = 1`, `panic = "abort"` set in `[profile.release]`. |

### 4.3 Reliability

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-REL-01 | Tests pass deterministically | `cargo test` is green on main with no flakiness. |
| NFR-REL-02 | No panics in composition root | Dependency wiring fails fast with a typed error, not a panic trace. |

### 4.4 Maintainability & Quality

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-MAINT-01 | Lint clean | `cargo clippy -- -D warnings` passes. |
| NFR-MAINT-02 | Format clean | `cargo fmt --check` passes. |
| NFR-MAINT-03 | Docs build | `cargo doc --no-deps` builds without errors. |
| NFR-MAINT-04 | Coverage intent | A `tarpaulin`-runnable setup is configured in CI stub (coverage gate introduced next milestone). |
| NFR-MAINT-05 | Dependency hygiene | `deny.toml` present; known-crates-only policy documented. |

### 4.5 Portability

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-PORT-01 | Tier-1 targets | Builds on Linux x86_64 and macOS aarch64. (Windows targeted post-v0.1.) |
| NFR-PORT-02 | No unsafe | Zero `unsafe` code in Domain/App layers in this milestone. |

### 4.6 Security

| ID | NFR | Acceptance |
|----|-----|------------|
| NFR-SEC-01 | No secrets in repo | `.gitignore` excludes `.env`, `target/`, `ag.toml.local`. No hardcoded credentials. |
| NFR-SEC-02 | Supply chain | `deny.toml` + `rust-advisory-db`-friendly audit configured for CI. |

---

## 5. Out of Scope

The scaffolding milestone must **resist scope creep**. The following are explicitly deferred and must **not** be delivered in v0.1.0:

1. **Actual LLM calls / OpenAI client wiring** — the LLM port exists, but no network token or completion logic.
2. **Natural-language task execution engine** — no chat loop, no prompt orchestration.
3. **Full file editing semantics** — no AST-aware edits, no diff/replace engine; only the `FileSystemPort` trait + std impl skeleton.
4. **Real shell session management** — only a `ShellPort` trait + `std::process::Command` stub; no persistent PTY sessions.
5. **Plugin loading runtime** — the `Plugin` entity and `PluginRegistryPort` are declared, but no dynamic library loading or WASM runtime.
6. **Persistent session state / history** — no on-disk session store.
7. **GUI / TUI rendering** — CLI only; no curses or ratatui integration in this milestone.
8. **CI pipeline files** — CI workflow `.yml` is out of scope; only a Makefile/`justfile` local runner.
9. **Packaging / releases** — no GitHub Release workflow, no Homebrew formula, no deb/rpm builds.
10. **Benchmarking actual agent workloads** — only a criterion placeholder crate; real benchmarks land later.
11. **Windows support** — not blocked, but not tested/required for this milestone.

---

## 6. Success Metrics

> Metrics are split by **Foundation Health** (the actual deliverable) and **Vision Leading Indicators** (forward-looking signals for the roadmap).

### 6.1 Primary (must-hit) — Foundation Health

| Metric | Target | Measurement |
|--------|--------|-------------|
| M1.1 Build green | `cargo build` succeeds, 0 warnings | CI / local |
| M1.2 Tests green | `cargo test` passes on a clean clone | CI / local |
| M1.3 Lint green | `cargo clippy -- -D warnings` + `cargo fmt --check` pass | CI / local |
| M1.4 Dependency cycle absence | `cargo-depgraph` shows acyclic, one-directional graph matching §3.4 | Run on CI |
| M1.5 Domain purity | `cargo tree -p domain` lists **no** non-stdlib crates | CI / local |
| M1.6 Version binary works | `cargo run --quiet -- version` prints version + git sha | Manual + script |

### 6.2 Secondary — Maintainability & Process

| Metric | Target | Measurement |
|--------|--------|------------|
| M2.1 Scaffolding PRs merged | ≤ 1 day median from PR open → merge | Repo stats |
| M2.2 New-contributor time-to-first-build | Documented steps ≤ 2 commands to a green build | Contributor survey |
| M2.3 Docs present | README, architecture doc, CONTRIBUTING exist | File listing check |

### 6.3 Leading Indicators — Vision Traction (forward-looking, not gates for this milestone)

| Metric | Threshold (early signal) | Note |
|--------|--------------------------|------|
| L1 Compile time | `cargo build --release` cold < 180s on CI runner | Signals healthy crate size |
| L2 Binary size | `ag version` release binary < 8 MB | Memory/efficiency proxy |
| L3 `cargo tree` edge count | Infra crates' direct deps ≤ 15 | Dependency complexity proxy |

> A milestone is considered **successful** when all of section 6.1 is green and section 6.3 leading indicators are within threshold.

---

## 7. Assumptions & Constraints

- The target audience for this milestone is **internal contributors**, not end users.
- The OpenCode feature set (§1.3) informs *what the entities/traits must eventually model*, but this milestone only scaffolds the structure, not behavior.
- Rust edition **2021** is used (not 2024, for max toolchain compatibility at milestone start).
- No GPU/CUDA involvement is assumed; the agent speaks to LLM providers over HTTP in later milestones.

---

## 8. Open Questions (to resolve during sprint)

1. **Crate granularity tradeoff:** Should `infra` be one monolithic crate or multiple small crates? (Recommended: multiple, per §3.1 — but confirm before final cut to keep compile times sane.)
2. **Git SHA embedding strategy:** `vergen` vs. build-script `git` — prefer `vergen gix` for hermetic builds; confirm offline CI feasibility.
3. **Error-handling library:** `anyhow` + `thiserror` vs. custom — Domain stays pure anyhow-free; App layer may adopt `anyhow` for orchestration glue. Decide before first use-case.
4. **Async runtime:** `tokio` (multi-thread) vs. `smol`/`async-std`. Recommend `tokio` for ecosystem fit, single-threaded by default for low memory until concurrency need arises. Confirm.

---

*End of document.*
