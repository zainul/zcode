# Task 06 — Infra: Shell Adapter

**Related PRD sections:** §3.1 infra, §3.2 ShellPort, Out-of-Scope #4
**Depends on:** task-02 (Domain)
**Status:** To do

## Objective
Implement `ShellPort` for `crates/infra/shell` using `std::process::Command`. `run` captures stdout; `spawn` is a stub returning `Err` (persistent PTY sessions deferred). Tests use a portable command (`echo`/`printf`).

## Step-by-step

1. Create `crates/infra/shell/Cargo.toml` — dep on `domain`.
2. Create `crates/infra/shell/src/lib.rs` exposing `StdShell`.
3. Implement `ShellPort::run`: parse `ShellCommand.command` via `sh -c` (Unix) / `cmd /C` (Windows), apply `cwd`, set `env` entries, capture `Stdio::piped`, enforce `timeout_ms` via `std::thread::spawn` + `join` (stub timeout not strictly required for v0.1 — document).
4. `spawn` returns `Err("pty sessions deferred to v0.2")` stub.
5. Add a unit test: `run("echo qagent")` stdout contains `qagent`.

## Test-case scenario
- `run` returns captured output for a trivial command; `spawn` fails cleanly with a typed error.

## How to verify
```
cargo test -p infra-shell
cargo clippy -p infra-shell -- -D warnings
```
**Pass criteria:** `run` test passes; stdout contains `qagent`; no shell-injection surface at this layer (input passed as a single argv to `sh -c`).

## Success metric mapping
- M1.2, M1.3, NFR-SEC-01 (no ambient shell=True in user input), NFR-REL-01.
