# Task 10 — Documentation Scaffold

**Related PRD sections:** FR-DOC-01..05, US-C-03
**Depends on:** task-01 (so crate map is stable)
**Status:** Done

## Objective
Produce the contributor-facing docs that make architecture, layer rules, and quick-start discoverable from a fresh clone.

## Step-by-step

1. Create root `README.md`:
   - One-liner + vision quote.
   - Mermaid crate-flow diagram (Interface → Infrastructure → App → Domain).
   - Layer dependency rules (the §3.4 table in one sentence each).
   - Quick-start: `cargo build`, `cargo test`, `cargo run -q -- version`.
   - Memory-efficiency note (owned types, no GC).
2. Create `docs/architecture/README.md`:
   - Crate map table (name | layer | purpose).
   - Dependency-direction diagram (Mermaid).
   - Ports-and-adapters rationale + future composition-root diagram.
3. Create `CHANGELOG.md` stub following "Keep a Changelog" (## [Unreleased] / ## [0.1.0] scaffolding).
4. Create `CONTRIBUTING.md` stub with build/test/lint commands and the `#![forbid(unsafe_code)]` rule scope.

## Test-case scenario
- A new contributor can run three commands and be productive; the crate graph in README matches `cargo tree`.

## How to verify
```
ls README.md docs/architecture/README.md CHANGELOG.md CONTRIBUTING.md
cargo doc --no-deps --workspace        # T5 (docs build)
```
**Pass criteria:** all four files exist; Mermaid diagram text contains `cli`, `app`, `domain`, `infra`; `cargo doc` builds.

## Success metric mapping
- M2.3 (docs present), US-C-03, NFR-MAINT-03 (docs build).
