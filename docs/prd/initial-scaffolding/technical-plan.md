# Technical Plan: Initial Project Scaffolding — zcode in Rust

**Plan ID:** TP-SCAFFOLD-001
**Derived from:** `docs/prd/initial-scaffolding/prd.md`
**Target:** zcode v0.1.0 — Clean Architecture foundation in Rust
**Lead Engineer:** Backend Team
**Status:** Final (ready for implementation)

---

## 1. Executive Summary

This plan scaffolds the **initial, non-behavioral** codebase for `zcode` (zcode), the Rust-native terminal coding agent that mirrors OpenCode's capabilities while cutting memory and cold-start cost. Success is **structural**: a compilable, tested, lint-clean, dependency-inverted Cargo workspace with seven functional crates and zero user-facing agent behavior.

No LLM calls, no chat loop, no PTY sessions, no plugin loading runtime ship in v0.1. The scaffolding delivers **traits, entities, a composition root, a `version` subcommand, and all quality gates** so future milestones inherit clean boundaries and a <300 ms cold start.

---

## 2. Resolved Architecture Decisions (from PRD §8 Open Questions)

| # | Question | Resolution | Rationale |
|---|----------|-----------|-----------|
| DQ1 | `infra` monolith vs. multiple crates? | **Multiple small crates** under `crates/infra/{name}` | Compile-unit isolation; a feature touching only the filesystem adapter recompiles only that crate, not a mega-infra; keeps `cargo tree` edge count low (L3). |
| DQ2 | Git SHA embedding strategy? | **`vergen gix`** via a build script in `crates/cli` | Hermetic, no `git` subprocess at build time; works in offline CI when `VERGEN_GIX_*`/`CARGO_ENCODED_RUSTFLAGS` are provided; fallback to `"unknown"` when git metadata unavailable. |
| DQ3 | Error-handling library? | **`thiserror`** in App + Infra; **manual `std::error::Error`** in Domain (stdlib only); **`anyhow`** permitted in the CLI composition root only. | Preserves **FR-DI-01** (Domain is dep-free) while giving App/Infra ergonomic typed errors and CLI a flexible glue layer. |
| DQ4 | Async runtime? | **`tokio`** — single-threaded current-thread runtime in CLI for v0.1 (`#[tokio::main(flavor = "current_thread")]`). | Smallest runtime footprint & fastest cold start (NFR-PERF-01); multi-thread flavor is an opt-in feature flag when concurrency scales (future milestone). |
| DQ5 | Owned vs. borrowed types in Domain? | **Owned by default**: `String`, `Box<[T]>`, `Vec<T>`, `PathBuf`. Avoid `&str` lifetimes in entity fields. | Eliminates lifetime propagation through use-cases (FR-PERF-03); the cost is a single heap allocation, which is acceptable and documented; hot-path `Box<[T]>` is preferred for fixed-length collections. |

---

## 3. Crate Topology & Dependency Flow

Enforced acyclic graph (direction = depends-on):

```
cli (composition root)  ──►  app  ──►  domain
cli  ──►  infra/config, infra/llm, infra/filesystem, infra/shell
cli  ──►  benches (criterion stub)
```

- `domain` → **stdlib only** (FR-DI-01).
- `app` → `domain` only (FR-DI-02).
- `infra/*` → `domain` + external crates (FR-DI-03).
- `cli` → all layers (FR-DI-04).
- `benches` → `criterion` + `domain` (criterion stub is opt-in dev artifact).

---

## 4. High-Level Changes

### 4.1 New workspace root
Create `Cargo.toml` (workspace manifest), `rust-toolchain.toml`, `.cargo/config.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.gitignore`.

### 4.2 Domain crate (`crates/domain`)
Pure stdlib crate: `entities` (`Task`, `FileEdit`, `ShellCommand`, `Plugin`, `AgentContext`, `TaskStatus`), `DomainError`, and five **port traits** (`LlmPort`, `FileSystemPort`, `ShellPort`, `PluginRegistryPort`, `LoggerPort`).

### 4.3 Application crate (`crates/app`)
`App` struct parameterized by trait objects of the ports; declares **use-case trait stubs** (`TaskRunner`, `EditPlanner`). Depends on `domain` only.

### 4.4 Infrastructure crates
- `crates/infra/llm` — `LlmPort` impl (OpenAI-compatible trait impl only; **no network token**, stubs stream type).
- `crates/infra/filesystem` — `FileSystemPort` impl backed by `std::fs`.
- `crates/infra/shell` — `ShellPort` impl backed by `std::process::Command`.
- `crates/infra/config` — config model + TOML/env loader (`zcode.toml`).

### 4.5 Interface crate (`crates/cli`)
`clap v4` CLI with `version` subcommand; **composition root** that wires concrete ports into `App`; embeds build metadata via `vergen gix`.

### 4.6 Performance & test hooks
`benches/` criterion stub crate; release profile `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbol"` (FR-PERF-01/03).

### 4.7 Documentation scaffold
`README.md`, `docs/architecture/README.md` (Mermaid crate graph), `CHANGELOG.md`, `CONTRIBUTING.md`.

### 4.8 Quality gates runner
`Makefile` exposing `build`, `test`, `lint`, `fmt`, `bench` plus a `make check-deps` that runs `cargo tree -p domain` to enforce pure-domains (FR-DI-05).

---

## 5. Low-Level Changes (file-by-file)

### 5.1 Workspace root

**`Cargo.toml`**
```toml
[workspace]
resolver = "2"
members = [
    "crates/domain",
    "crates/app",
    "crates/infra/llm",
    "crates/infra/filesystem",
    "crates/infra/shell",
    "crates/infra/config",
    "crates/cli",
    "benches",
]
exclude = ["target"]

[workspace.package]
name = "zcode"
version = "0.1.0"
edition = "2021"
rust-version = "1.80"
license = "MIT OR Apache-2.0"
repository = "https://github.com/zainul/zcode"

[workspace.dependencies]
tokio = { version = "1.39", default-features = false, features = ["rt"] }
clap = { version = "4.5", features = ["derive"] }
toml = "0.8"
serde = { version = "1.0", features = ["derive"] }
thiserror = "1.0"
vergen-gix = { version = "1.0", default-features = false, features = ["build", "cargo"] }
vergen-config = "0.1"
log = "0.4"
env_logger = "0.11"

[profile.release]
lto = "thin"
codegen-units = 1
panic = "abort"
strip = "symbol"
opt-level = 3

[profile.dev.package."*"]
opt-level = 2

[profile.ci]
inherits = "dev"
opt-level = 0
```

**`rust-toolchain.toml`**
```toml
[toolchain]
channel = "1.80.0"
components = ["rustfmt", "clippy"]
```

**`.cargo/config.toml`**
```toml
[build]
target-dir = "target"

[term]
quiet-workspaces = true

[net]
git-fetch-with-cli = true
```

**`rustfmt.toml`**
```toml
edition = "2021"
max_width = 100
tab_spaces = 4
wrap_comments = true
format_code_in_doc_items = true
```

**`clippy.toml`**
```toml
msrv = "1.80.0"
disallowed-methods = []
```

**`deny.toml`**
```toml
[advisories]
ignore = []

[licenses]
allow-osi-fsf-free = "either"
copyleft = "deny"
confidence-threshold = 0.8

[bans]
multiple-versions = "allow"
wildcards = "allow"
```

**`.gitignore`**
```
/target
**/*.rs.bk
.env
zcode.toml.local
.coverage/
*.profraw
```

### 5.2 Domain crate — `crates/domain`

**`Cargo.toml`**
```toml
[package]
name = "domain"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
# Intentionally empty: stdlib only (FR-DI-01)
```

**`src/lib.rs`**
```rust
//! Domain layer — core business rules, entities, domain errors, and port traits.
//! Pure stdlib: zero third-party dependencies (enforced by FR-DI-01).

pub mod error;
pub mod model;
pub mod ports;

pub use error::DomainError;
pub use model::{
    AgentContext, FileEdit, Plugin, ShellCommand, Task, TaskStatus,
};
pub use ports::{FileSystemPort, LlmPort, LoggerPort, PluginRegistryPort, ShellPort};
```

**`src/model.rs`** — owned entities + boxed collections.
```rust
use std::path::PathBuf;

/// Ownership rule: all entity fields are owned (`String`/`PathBuf`/`Box<[T]>`)
/// to avoid lifetime propagation through use-cases (FR-PERF-03).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub constraints: Box<[String]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub old_content: String,
    pub new_content: String,
}

#[derive(Clone, Debug)]
pub struct ShellCommand {
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub entrypoint: PathBuf,
}

#[derive(Clone, Debug)]
pub struct AgentContext {
    pub working_dir: PathBuf,
    pub model: String,
    pub env: Vec<(String, String)>,
}
```

**`src/error.rs`** — manual std impl (no thiserror, to keep dep-free).
```rust
use std::fmt;

#[derive(Debug)]
pub enum DomainError {
    InvalidInput(String),
    NotFound(String),
    Conflict(String),
    Invariant(String),
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(m) => write!(f, "invalid input: {m}"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::Conflict(m) => write!(f, "conflict: {m}"),
            Self::Invariant(m) => write!(f, "invariant violated: {m}"),
        }
    }
}

impl std::error::Error for DomainError {}
```

**`src/ports.rs`** — the dependency-inversion boundary.
```rust
use std::path::{Path, PathBuf};

/// Stream type returned by LLM completions. Resolved to a boxed iterator over
/// result-chunks in infra; kept abstract here so Domain is async-agnostic.
pub struct CompletionChunk {
    pub delta: String,
    pub done: bool,
}

pub trait LlmPort {
    fn send(&mut self, system: &str, prompt: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
    fn stream<'a>(&'a mut self, system: &'a str, prompt: &'a str)
        -> Box<dyn Iterator<Item = Result<CompletionChunk, Box<dyn std::error::Error + Send + Sync>>> + 'a>;
}

pub trait FileSystemPort {
    fn read(&self, path: &Path) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
    fn write(&self, path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn list(&self, path: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error + Send + Sync>>;
    fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
    fn watch(&self, path: &Path) -> Result<Box<dyn std::error::Error + Send + Sync>, Box<dyn std::error::Error + Send + Sync>>; // stub sig
}

pub trait ShellPort {
    fn spawn(&mut self, cmd: &ShellCommand) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn run(&mut self, cmd: &ShellCommand) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

pub trait PluginRegistryPort {
    fn discover(&self) -> Result<Vec<Plugin>, Box<dyn std::error::Error + Send + Sync>>;
    fn load(&self, plugin: &Plugin) -> Result<(), Box<dyn std::error::Error + Send + Sync>>; // stub
    fn execute(&self, plugin: &Plugin, input: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

pub trait LoggerPort {
    fn log(&self, level: LogLevel, msg: &str);
    fn with_field(&self, key: &str, value: &str) -> Box<dyn LoggerPort + Send + Sync>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel { Trace, Debug, Info, Warn, Error }
```

### 5.3 Application crate — `crates/app`

**`Cargo.toml`**
```toml
[package]
name = "app"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
domain = { path = "../domain", version = "0.1.0" }
thiserror = { workspace = true }
```

**`src/lib.rs`**
```rust
//! Application layer — use-case orchestration over Domain ports.
//! Depends on `domain` only (FR-DI-02); no concrete infra crates.

use std::sync::Arc;

use domain::{
    AgentContext, FileEdit, FileSystemPort, LlmPort, LoggerPort, PluginRegistryPort, ShellCommand,
    ShellPort,
};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("port resolution failed: {0}")]
    Port(String),
    #[error("{0}")]
    Domain(#[from] domain::DomainError),
}

/// A use-case trait. Concrete orchestration ships next milestone; here we
/// declare the contract so the composition root can wire it later.
pub trait TaskRunner {
    fn run(&self, ctx: &AgentContext, task: &domain::Task) -> Result<(), AppError>;
}

/// A use-case trait for planning edits against domain `FileEdit` values.
pub trait EditPlanner {
    fn plan(&self, ctx: &AgentContext, edit: &FileEdit) -> Result<(), AppError>;
}

/// Orchestrator holding boxed port trait-objects.
pub struct App<const N: usize = 4> {
    llm: Arc<dyn LlmPort + Send + Sync>,
    fs: Arc<dyn FileSystemPort + Send + Sync>,
    shell: Arc<dyn ShellPort + Send + Sync>,
    plugins: Arc<dyn PluginRegistryPort + Send + Sync>,
    logger: Arc<dyn LoggerPort + Send + Sync>,
}

impl App {
    pub fn new(
        llm: Arc<dyn LlmPort + Send + Sync>,
        fs: Arc<dyn FileSystemPort + Send + Sync>,
        shell: Arc<dyn ShellPort + Send + Sync>,
        plugins: Arc<dyn PluginRegistryPort + Send + Sync>,
        logger: Arc<dyn LoggerPort + Send + Sync>,
    ) -> Self {
        Self { llm, fs, shell, plugins, logger }
    }
}

impl TaskRunner for App {
    fn run(&self, _ctx: &AgentContext, _task: &domain::Task) -> Result<(), AppError> {
        Err(AppError::Port("task engine not implemented in v0.1.0".into()))
    }
}

impl EditPlanner for App {
    fn plan(&self, _ctx: &AgentContext, _edit: &FileEdit) -> Result<(), AppError> {
        Err(AppError::Port("edit planner not implemented in v0.1.0".into()))
    }
}
```
> Note: the generic `const N` is a reserved future hook (planned plugin count bound). It is unused but harmless; clippy is disabled for it via `#[allow(dead_code)]` on the struct.

### 5.4 Infrastructure crates

**`crates/infra/llm/Cargo.toml`**
```toml
[package]
name = "infra-llm"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
thiserror.workspace = true
```
**`src/lib.rs`** — OpenAI-compatible trait impl stub:
```rust
//! OpenAI-compatible LLM adapter implementing `domain::LlmPort`.
//! v0.1 delivers the trait impl shape only; no network calls/secrets (§5 Out of Scope).

use domain::ports::CompletionChunk;
use std::error::Error;

pub struct OpenAiLlm {
    pub endpoint: String,
    pub model: String,
}

impl OpenAiLlm {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self { endpoint: endpoint.into(), model: model.into() }
    }
}

impl domain::LlmPort for OpenAiLlm {
    fn send(&mut self, _system: &str, _prompt: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        Err("llm network disabled in v0.1.0".into())
    }
    fn stream<'a>(&'a mut self, _system: &'a str, _prompt: &'a str)
        -> Box<dyn Iterator<Item = Result<CompletionChunk, Box<dyn Error + Send + Sync>>> + 'a>
    {
        Box::new(std::iter::once(Ok(CompletionChunk {
            delta: String::new(),
            done: true,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stub_does_not_call_network() {
        let mut llm = OpenAiLlm::new("http://localhost:9999", "gpt-4");
        let res = llm.send("sys", "hi");
        assert!(res.is_err());
    }
}
```

**`crates/infra/filesystem`** — `std::fs` impl of `FileSystemPort` (read/write/list/exists/watch). Tests use `tempfile` under `#[cfg(test)]`.

**`crates/infra/shell`** — `std::process::Command`-wrapped `ShellPort::run`/`spawn`. Tests via `echo` on Unix.

**`crates/infra/config`** — `ConfigSchema` serde struct + `Loader` combining `std::env` and `zcode.toml` (via `toml`). Default `zcode.toml` example committed under `crates/infra/config/examples/zcode.example.toml`.

### 5.5 Interface crate — `crates/cli`

**`Cargo.toml`**
```toml
[package]
name = "zcode"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
domain = { path = "../domain", version = "0.1.0" }
app = { path = "../app", version = "0.1.0" }
infra-llm = { path = "../infra/llm", version = "0.1.0" }
infra-filesystem = { path = "../infra/filesystem", version = "0.1.0" }
infra-shell = { path = "../infra/shell", version = "0.1.0" }
infra-config = { path = "../infra/config", version = "0.1.0" }
clap = { workspace = true }
tokio = { workspace = true }
log = { workspace = true }
env_logger = { workspace = true }

[build-dependencies]
vergen-gix.workspace = true
vergen-config = "0.1"
```

**`build.rs`**
```rust
fn main() {
    let git_cfg = vergen_config::build::get_git_timestamp()
        .unwrap_or_default()
        .as_millis();
    vergen_gix::gix::add_instructions(&git_cfg)
        .expect("failed to generate git metadata");
}
```
> Simpler robust variant uses `vergen-gix` `build` feature + `vergen-config` to emit `VERGEN_GIX_SHA`, `CARGO_PKG_VERSION`. The CLI reads these `env!()`s at compile time.

**`src/main.rs`** — tokio single-thread entry:
```rust
#![forbid(unsafe_code)]

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if let Err(e) = zcode::cli::run().await {
        eprintln!("zcode: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
```

**`src/lib.rs`** exports `pub mod cli;` so integration tests can call `zcode::cli::run()`.

**`src/cli/mod.rs`** — `clap` CLI + composition root:
```rust
use clap::{CommandFactory, Parser};
use std::sync::Arc;

/// CLI definition parsed with clap v4 derive (FR-CLI-02).
#[derive(Parser)]
#[command(name = "zcode", version, about = "zcode — the lean Rust coding agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::Subcommand)]
pub enum Commands {
    /// Print build metadata (FR-CLI-01).
    Version,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("VERGEN_GIX_SHA", "unknown");
pub const BUILD_PROFILE: &str = env!("VERGEN_BUILD_PROFILE", "unknown");

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::try_parse()?;
    match cli.command {
        Commands::Version => {
            println!("zcode v{} (git: {}, profile: {})", VERSION, GIT_SHA, BUILD_PROFILE);
            Ok(())
        }
    }
}
```

> **Composition root wiring** (`wire()` helper, called by future non-version commands): construct concrete `OpenAiLlm`, `StdFs`, `StdShell`, `TomlConfigLoader`, `EnvLogger` → wrap in `Arc<dyn Port>` → `App::new(...)`. On a port that cannot be resolved, return `AppError::Port` and fail fast (NFR-REL-01) rather than panic with a trace.

### 5.6 Performance & bench hooks

**`benches/Cargo.toml`**
```toml
[package]
name = "zcode-benches"
version.workspace = true
edition.workspace = true
publish = false

[dependencies]
domain = { path = "../crates/domain", version = "0.1.0" }
criterion = "0.5"

[[bench]]
name = "smoke"
harness = false

[lib]
bench = false
```

**`benches/benches/smoke.rs`** — criterion placeholder measuring Domain entity construction latency.

### 5.7 Documentation scaffold
`README.md`, `docs/architecture/README.md` (Mermaid crate graph + contributor rules), `CHANGELOG.md` (Keep a Changelog stub), `CONTRIBUTING.md`.

### 5.8 Quality gates runner
`Makefile` with `build`, `test`, `lint`, `fmt`, `bench`, `check-deps` targets (§5.8).

---

## 6. Testing Strategy & Verification Scenarios

| # | Scenario | Target crate | Method | Expected |
|---|----------|--------------|--------|----------|
| T1 | Workspace compiles clean | workspace | `cargo build` | exit 0, 0 warnings |
| T2 | Tests green | all | `cargo test --workspace` | all pass, deterministic |
| T3 | Lint clean | all | `cargo clippy --workspace -- -D warnings` | exit 0 |
| T4 | Format clean | all | `cargo fmt --check` | no diff |
| T5 | Docs build | all | `cargo doc --no-deps` | exit 0 |
| T6 | Domain purity | `domain` | `cargo tree -p domain` | no `[j`-`/cargo:` lines |
| T7 | Dependency acyclicity | workspace | `cargo-depgraph` script | matches §3 graph |
| T8 | Version runs | `cli` | `cargo run -q -- version` | prints `zcode v0.1.0 (git: …, profile: …)` |
| T9 | Cold start < 300 ms | `cli` | `time cargo run --release -- version` | wall < 300 ms (measured next milestone) |
| T10 | Release binary size | `cli` | `ls -la target/release/zcode` | < 8 MB (leading indicator L2) |
| T11 | LLM stub no network | `infra-llm` | unit test | `send()` returns `Err` |
| T12 | Filesystem impl round-trip | `infra-filesystem` | tempdir read/write/list/exists | round-trip ok, idempotent |
| T13 | Shell impl runs | `infra-shell` | `run("echo qagent")` | stdout contains `qagent` |

### 6.1 Verify Results (command matrix)
```
cargo build                              # T1
cargo test --workspace                    # T2
cargo clippy --workspace -- -D warnings   # T3
cargo fmt --check                        # T4
cargo doc --no-deps --workspace          # T5
cargo tree -p domain                     # T6
cargo run --quiet -- version             # T8
```

---

## 7. Success Metrics & Acceptance Criteria (from PRD §6)

### Primary gates (must-hit)
| Metric | Target | Verified by |
|--------|--------|-------------|
| M1.1 Build green | `cargo build` 0 warnings | T1 |
| M1.2 Tests green | `cargo test` green | T2 |
| M1.3 Lint green | `clippy -D warnings` + `fmt --check` | T3, T4 |
| M1.4 Acyclic graph | matches §3 | T7 |
| M1.5 Domain purity | no third-party deps | T6 |
| M1.6 Version binary | prints version + sha | T8 |

### Secondary
| Metric | Target | Verified by |
|--------|--------|-------------|
| M2.3 Docs present | README + arch doc + CONTRIBUTING | file check |

### Leading indicators
| Metric | Threshold | Verified by |
|--------|-----------|-------------|
| L1 Compile time | `cargo build --release` cold < 180 s | manual |
| L2 Binary size | release `zcode` < 8 MB | T10 |
| L3 Dep edges | infra direct deps ≤ 15 | `cargo tree` |

---

## 8. Requirements Traceability

### Functional (§3 of PRD)
| FR ID | How satisfied |
|-------|---------------|
| FR-CLI-01..04 | §5.5 CLI composition root + `vergen-gix` build.rs |
| FR-DI-01..05 | §5.2 domain dep-free + `make check-deps`; §3 topology |
| FR-TOOL-01..06 | §5.1 root manifests + §5.8 Makefile |
| FR-DOC-01..05 | §5.7 documentation scaffold |
| FR-PERF-01..04 | `[profile.release]` in §5.1 + §5.6 benches |

### Non-functional (§4 of PRD)
| NFR ID | Acceptance |
|--------|------------|
| NFR-BUILD-01..03 | §5.1 `rust-toolchain.toml`, minimal features, `.cargo/config` |
| NFR-PERF-01..03 | current-thread runtime, `lto="thin"`, `panic="abort"` (§5.1, §5.5) |
| NFR-REL-01..02 | typed `AppError::Port`, fail-fast (§5.3) |
| NFR-MAINT-01..05 | §5.8 gates; `deny.toml` (§5.1) |
| NFR-PORT-01..02 | Tier-1 targets; `#![forbid(unsafe_code)]` in `cli` (§5.5) |
| NFR-SEC-01..02 | `.gitignore` + `deny.toml` (§5.1) |

---

## 9. Observability, Reliability, Stability & Security

### Observability
- `env_logger` wired in `cli`; `LoggerPort` trait in Domain gives future structured logs without coupling (§5.2).
- `vergen-gix` build metadata enables traceable binaries (release + sha) (§5.5).
- Criterion smoke bench provides the first regression hook (§5.6).

### Reliability
- Composition root validates every port at startup; missing port → typed `AppError::Port`, exit code 1 — **no panic traces** (NFR-REL-01/02).
- `cargo test` is green & deterministic; CI parity via `rust-toolchain.toml` pin (M1.2).
- `rt-multi-thread` is opt-in; v0.1 uses `current_thread` to avoid idle-thread memory (NFR-PERF-01).

### Stability
- `deny.toml` bans copyleft licenses & enforces advisory checks (NFR-SEC-02).
- `#[forbid(unsafe_code)]` is on `cli`; allowed in domain/app via lint but zero `unsafe` there (NFR-PORT-02).
- `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbol"` minimize binary bloat & abort cost (NFR-PERF-03).

### Security
- `.gitignore` excludes `.env`, `zcode.toml.local`, `target/`; no hardcoded secrets (NFR-SEC-01).
- LLM adapter deliberately exposes **no API key field** wired to network in v0.1 (Out of Scope); config loader stores secrets only in-process env, never written to disk (§5.4).
- Supply-chain `deny.toml` (§5.1) gates dependencies to approved licenses.
- Filesystem/Shell adapters operate on explicit `Path`/`ShellCommand` inputs — no ambient shell=True (no shell-injection surface at this layer).

---

## 10. Implementation Roadmap (task files)

Detailed step-by-step, test-case, and verification are in per-task documents under `docs/prd/initial-scaffolding/tasks/`:

| Task | Artifact |
|------|----------|
| task-01 | Workspace & tooling configuration |
| task-02 | Domain crate (entities, error, ports) |
| task-03 | Application crate (orchestrator + use-case traits) |
| task-04 | Infra: LLM adapter |
| task-05 | Infra: Filesystem adapter |
| task-06 | Infra: Shell adapter |
| task-07 | Infra: Configuration model + loader |
| task-08 | CLI composition root + version subcommand |
| task-09 | Performance & bench hooks |
| task-10 | Documentation scaffold |
| task-11 | Build verification & quality-gate runner |

**Ordering constraint:** tasks 01–02 must land first (domain is the dependency root). Task 08 must land after 02–07 (wiring). Tasks 09–11 are independent and may run in parallel with 04–07.

---

*End of technical plan.*
