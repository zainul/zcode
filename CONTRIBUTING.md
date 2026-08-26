# Contributing to zcode

Thanks for your interest in contributing! This guide covers building, testing, and linting.

## Prerequisites

- Rust **1.85.0** (pinned via `rust-toolchain.toml`)
- Components: `rustfmt`, `clippy` (auto-installed by rustup)

## Quick start

```sh
cargo build                    # build workspace
cargo test                     # run all tests
cargo run -q -- version        # print version + git sha
```

## Quality gates

All gates are unified under the `Makefile`:

| Command          | Description                              |
|------------------|------------------------------------------|
| `make ci`        | `fmt-check` + `clippy` + `test` + `build`|
| `make fmt`       | Format code                              |
| `make fmt-check` | Verify formatting                        |
| `make lint`      | Clippy with `-D warnings`                |
| `make test`      | Run all tests                            |
| `make build`     | Build workspace                          |
| `make bench`     | Run criterion benchmarks                 |
| `make check-deps`| Verify Domain has zero third-party deps  |

## Layer rules

- **Domain** (`crates/domain`) must have **zero** third-party dependencies (`make check-deps`).
- **App** (`crates/app`) depends on `domain` + `thiserror` only — no tokio, no serde, no HTTP.
- **Infra/** crates and `crates/tools` depend on `domain` + external crates.
- **CLI** (`crates/cli`) is the composition root — depends on all layers.

Run `bash docs/architecture/dependency-check.sh` to verify the whole graph.

## Safety

- `#[forbid(unsafe_code)]` is enforced in `crates/cli` and every infra crate.
- There is no `unsafe` anywhere in the workspace.
- Network-touching tests are `#[ignore]`d so `cargo test --workspace` stays
  hermetic and deterministic. Run them with `cargo test -- --ignored`.

## Style

- Follow `rustfmt.toml` formatting (run `cargo fmt` before committing).
- All clippy warnings are errors (`-D warnings`).
- Write tests for new functionality.
