# Task 05 — Infra: Filesystem Adapter

**Related PRD sections:** §3.1 infra, §3.2 FileSystemPort, Out-of-Scope #3
**Depends on:** task-02 (Domain)
**Status:** Done

## Objective
Implement `FileSystemPort` for `crates/infra/filesystem` backed by `std::fs`. Provides `read`, `write`, `list`, `exists`, `watch` (watch is a stub). Tests use `tempfile` under `#[cfg(test)]` to be hermetic.

## Step-by-step

1. Create `crates/infra/filesystem/Cargo.toml` — dep on `domain` + `tempfile` (dev-dep) + `thiserror` (workspace).
2. Create `crates/infra/filesystem/src/lib.rs` exposing `StdFs`.
3. Implement `read` (file → `String` via `read_to_string`), `write` (`create + write_all`), `list` (`read_dir` → `Vec<PathBuf>`), `exists` (`Path::exists`). `watch` returns a boxed error stub.
4. Add integration tests in `tests/fs.rs` (or `#[cfg(test)]` mod): create tempdir, write a file, read it back, list dir, assert equality.

## Test-case scenario
- A file written to a temp dir is read back byte-identical; listing returns expected entries; missing file `read` returns an error.

## How to verify
```
cargo test -p infra-filesystem
cargo clippy -p infra-filesystem -- -D warnings
```
**Pass criteria:** round-trip test passes; `read` on a missing path returns `Err`; `tempfile` is a dev-dep only (not in release `cargo tree`).

## Success metric mapping
- M1.2, M1.3, NFR-REL-01 (errors as `Result`, no panics on missing file).
