# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Workspace scaffolding for the QAgent (`ag`) Rust coding agent.
- Domain crate with entities (`Task`, `FileEdit`, `ShellCommand`, `Plugin`, `AgentContext`), `DomainError`, and port traits.
- Application crate with `App` orchestrator and `TaskRunner`/`EditPlanner` use-case traits.
- Infrastructure crates: LLM adapter (stub), Filesystem adapter (std::fs), Shell adapter (std::process), Configuration loader (TOML + env).
- CLI crate with `version` subcommand, composition root (`wire()`), and single-threaded tokio runtime.
- Criterion smoke benchmark for domain entity construction.
- `Makefile` quality-gate runner with `ci`, `test`, `lint`, `fmt`, `bench`, `check-deps` targets.
- Documentation scaffold (README, architecture guide, CHANGELOG, CONTRIBUTING).

## [0.1.0] - 2026-08-24

### Scaffolding

- Initial project scaffolding milestone complete.
- No LLM calls, no chat loop, no PTY sessions — pure structural foundation.
- All quality gates green: `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`, `cargo doc`.
