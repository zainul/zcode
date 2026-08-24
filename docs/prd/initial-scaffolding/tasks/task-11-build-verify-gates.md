# Task 11 — Build Verification & Quality-Gate Runner

**Related PRD sections:** FR-TOOL-04/05, FR-DI-05, M1.1..M1.6, NFR-MAINT-01..05
**Depends on:** all other tasks (final gate)
**Status:** To do

## Objective
Provide a single `Makefile` entrypoint (`make test`, `make lint`, `make fmt`, `make build`, `make bench`, `make check-deps`) and a dependency-direction assertion script so CI and contributors share one source of truth for quality gates.

## Step-by-step

1. Create `Makefile` with targets:
   - `build` → `cargo build --workspace`
   - `test` → `cargo test --workspace`
   - `lint` → `cargo clippy --workspace -- -D warnings`
   - `fmt` → `cargo fmt` (and `fmt-check` → `cargo fmt --check`)
   - `bench` → `cargo bench`
   - `check-deps` → `cargo tree -p domain | grep -Eq '(cargo:|[j)' && echo "DOMAIN IMPURE" && exit 1 || echo "domain pure OK"` (enforces FR-DI-01)
   - `ci` → `fmt-check lint test build`
2. (Optional) add `cargo-depgraph` script snippet under `docs/architecture/dependency-check.sh` validating §3.4 acyclicity (FR-DI-05).
3. Run the full gate locally.

## Test-case scenario
- `make ci` ends green on a fresh checkout; `make check-deps` proves Domain purity.

## How to verify
```
make ci                  # fmt-check + clippy + test + build   (M1.3, M1.1, M1.2)
make check-deps         # asserts FR-DI-01 / M1.5
make build              # clean (M1.1)
cargo run --quiet -- version   # passes (M1.6)
```
**Pass criteria:** `make ci` exits 0; `make check-deps` reports `domain pure OK`; version subcommand works; full `cargo tree` confirms acyclicity (M1.4).

## Success metric mapping
- M1.1, M1.2, M1.3, M1.4, M1.5, M1.6 (all primary gates).
