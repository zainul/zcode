# QAgent (`ag`)

> A lean, fast, and memory-safe terminal coding agent written in Rust.

## Vision

QAgent is a Rust-native terminal coding agent that mirrors [OpenCode](https://github.com/sst/opencode)'s core capabilities — LLM interaction, file system operations, shell command execution, and plugin architecture — while cutting memory usage and cold-start cost through idiomatic Rust, zero-cost abstractions, and a thin dependency footprint.

> *"Build for correctness first, performance always."*

## Architecture

```
Interface (cli) → Application (app) → Domain (domain)
Interface (cli) → Infrastructure (infra/*) → Domain (domain)
```

### Layer dependency rules

- **Domain** → stdlib only (zero third-party deps).
- **App** → depends on `domain` only.
- **Infra/\*** → depend on `domain` + external crates.
- **CLI** → composition root; depends on all layers.

### Crate map

| Crate                | Layer       | Purpose                                  |
|----------------------|-------------|------------------------------------------|
| `crates/domain`      | Domain      | Entities, errors, port traits            |
| `crates/app`         | Application | Use-case orchestration (`App`, `TaskRunner`) |
| `crates/infra/llm`   | Infra       | OpenAI-compatible LLM adapter            |
| `crates/infra/fs`    | Infra       | Filesystem adapter (`std::fs`)           |
| `crates/infra/shell` | Infra       | Shell adapter (`std::process`)           |
| `crates/infra/config`| Infra       | TOML + env configuration loader          |
| `crates/cli`         | Interface   | `clap` CLI + composition root            |

## Quick start

```sh
cargo build                    # build workspace
cargo test                     # run all tests
cargo run -q -- version        # print version + git sha
```

## Memory efficiency

All Domain entities use owned types (`String`, `PathBuf`, `Box<[T]>`) to avoid lifetime propagation through use-cases. No garbage collector, no runtime, no GC pauses. The CLI uses a single-threaded tokio runtime for minimal idle-thread memory.

## License

Dual-licensed under MIT or Apache-2.0.
