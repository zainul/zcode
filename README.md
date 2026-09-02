# zcode

> A lean, fast, and memory-safe terminal coding agent written in Rust.

## Vision

zcode is a Rust-native terminal coding agent that mirrors [OpenCode](https://github.com/sst/opencode)'s core capabilities — LLM interaction, file system operations, shell command execution, MCP/LSP extensibility — while cutting memory usage and cold-start cost through idiomatic Rust, zero-cost abstractions, and a thin dependency footprint.

> *"Build for correctness first, performance always."*

## Quick start

```sh
git clone <this-repo> && cd zcode
./scripts/install.sh              # detects your platform, builds, installs
zcode version
```

`./scripts/install.sh --help` covers `--prefix`, `--no-build` and the rest.
To update later: `git pull && ./scripts/update.sh` — it replaces the binary
where it already lives and prints the old and new build stamps.
`./scripts/uninstall.sh` removes it again. Prefer to do it by hand?
`cargo build --release` and copy `target/release/zcode` anywhere on your `PATH`.

Point it at a provider. Configuration lives in `zcode.json` or `zcode.toml`
(JSON wins if both are present); keys are read from the environment by name and
are never written to disk.

```sh
cp crates/infra/config/examples/zcode.example.json zcode.json
export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...

zcode run "add a doc comment to every public fn in crates/domain/src/model.rs"
zcode run --json "list the files in crates"     # JSONL for scripts/CI
zcode run --mode planning "how would you split this crate?"
zcode run -m openrouter/z-ai/glm-4.6 "…"        # <provider>/<model>, opencode style
zcode                                           # interactive TUI
```

The smallest possible config — everything else has a per-provider default:

```json
{ "provider": "openrouter", "model": "anthropic/claude-sonnet-4.5" }
```

Supported providers: `openrouter`, `openai`, `anthropic`, `deepseek`,
`ollama`, `vllm`, and any `openai-compatible` endpoint via `base_url`. Omit
`model` and each provider gets a working default; omit `api_key_env` and the
conventional variable for that provider is used
(`ZCODE_OPENROUTER_API_KEY`, `ZCODE_ANTHROPIC_API_KEY`, `ZCODE_DEEPSEEK_API_KEY`, …).

**New here? The [user guide](docs/guide/README.md) walks through installation
and every feature step by step.**

## Commands

| Command | Interface | Purpose |
|---------|-----------|---------|
| `zcode version` | headless | Build metadata (version, git SHA, profile). |
| `zcode run "<prompt>"` | headless | One agent run. `--json`, `--json-format`, `--mode`, `--provider`, `--model`/`-m`, `--image`, `--session`, `--timeout`, `--config`. |
| `zcode` / `zcode repl` | TUI | Interactive session with live tool output. `--mode`, `--provider`, `--model`/`-m`, `--session`, `--config`. |
| `zcode session create` | headless | Allocate a session id (UUIDv7). |
| `zcode session continue <id> [prompt]` | headless/TUI | Resume; without a prompt it opens the TUI. |
| `zcode session fork <id> [--as <new>]` | headless | Branch a transcript. |
| `zcode session import <file>` / `export <id> --to <file>` | headless | Portable JSON sessions. |
| `zcode tools list` | headless | Every tool: native + `mcp__*` + `lsp__*`. |
| `zcode config` | headless | Which config files are in use and what they resolve to. |
| `zcode skills list` | headless | Markdown skills in the skills dir. |

## Architecture

```
Interface (cli) → Application (app) → Domain (domain)
Interface (cli) → Infrastructure (infra/*, tools) → Domain (domain)
```

### Layer dependency rules

- **Domain** → stdlib only (zero third-party deps, enforced by `make check-deps`).
- **App** → depends on `domain` only.
- **Infra/\*** and **tools** → depend on `domain` + external crates.
- **CLI** → composition root; depends on all layers.

### Crate map

| Crate | Layer | Purpose |
|-------|-------|---------|
| `crates/domain` | Domain | Entities, errors, port traits, mode policy, tool-name canonicalisation |
| `crates/app` | Application | The agent loop (`AgentLoop::execute`): stream → tool-use → checkpoint → repeat |
| `crates/tools` | Infra | Native tools + allowlisted shell + the merging `ToolRegistry` |
| `crates/infra/llm` | Infra | OpenAI / OpenRouter / Anthropic / DeepSeek / Ollama / vLLM streaming clients |
| `crates/infra/mcp` | Infra | MCP stdio JSON-RPC client |
| `crates/infra/lsp` | Infra | LSP stdio JSON-RPC client |
| `crates/infra/filesystem` | Infra | Filesystem adapter (`std::fs`), atomic writes |
| `crates/infra/shell` | Infra | Shell adapter (`std::process`) |
| `crates/infra/session` | Infra | UUIDv7 session store, atomic checkpoints, import/export |
| `crates/infra/telemetry` | Infra | JSONL event stream + run report |
| `crates/infra/config` | Infra | TOML + `ZCODE_*` env configuration loader |
| `crates/cli` | Interface | `clap` CLI, ratatui TUI, composition root |

### The tool namespace

Native tools have bare names — `read`, `write`, `str_replace_editor`,
`apply_patch`, `list_dir`, `shell`, `zcode_skill`. MCP tools appear as `mcp__<server>__<tool>`
and LSP tools as `lsp__goto_definition`, `lsp__find_references`, `lsp__hover`,
`lsp__rename_symbol`. `__` rather than `::` because provider function-calling
APIs only accept `[A-Za-z0-9_-]`; the `::` spellings still resolve as aliases.

Adding a tool means implementing `domain::Tool` and registering it — the engine
needs no changes.

## Streaming and telemetry

Responses are decoded incrementally: the first token reaches the screen as soon
as the provider emits it, not when the generation ends. Every run records the
model name, input/output/cache tokens, step count, and wall-clock time, and
writes them to `.zcode/reports/<timestamp>-<session>.json`. With `--json` the same
events stream to stdout as JSONL, one object per line:

```
{"kind":"loop_start","model":"anthropic/claude-sonnet-4.5",...}
{"kind":"tool_call","tool":"apply_patch",...}
{"kind":"tool_result","tool":"apply_patch","truncated":false,...}
{"kind":"llm_delta","text":"Done — ",...}
{"kind":"finish","input_tokens":320,"output_tokens":42,"cache_tokens":10,"steps":2,...}
```

Transient provider failures (429, 5xx) are retried with exponential backoff and
honour `Retry-After`; authentication and model-name errors fail immediately with
the provider's own message.

## Safety

- Every shell command is checked against the `shell_allowed` regex allowlist
  **before** it reaches `std::process::Command`. An empty list denies
  everything. Under a narrow allowlist, substitution/redirection/chaining
  (`` ` ``, `$(`, `>`, `&`) is refused outright; a pattern that already matches
  every command (`".*"`) lifts that check, since there is nothing left to
  smuggle past it.
- A separate always-on denylist refuses the irreversible and the escalating
  (`sudo`, `dd … of=`, `git push --force`, `curl … | sh`) whatever the
  allowlist says. Recursive deletes are judged by their *target*, not their
  flag: `rm -rf node_modules` runs, `rm -rf ~` and `rm -rf $VAR` do not.
- Planning mode withholds every editing tool from the model and refuses one if
  it is requested anyway.
- API keys are read from the env var named by `api_key_env` at wiring time and
  never written to disk.
- Tool output is stripped of terminal escapes before display and capped at
  `max_tool_output_chars` before it enters the transcript.

## Token efficiency

Shell output is routed through [rtk](https://github.com/rtk-ai/rtk) when it is
available — a CLI proxy that filters what a command returns before it reaches
the model. `ls -la` comes back 87% smaller, `git status` 59%. It is on by
default, and zcode installs it (via Homebrew or Cargo, never a piped script) if
it is missing. Everything that enters the transcript is also capped at
`max_tool_output_chars`, and the cost of a session is estimated live from a
built-in price table.

## Memory efficiency

No garbage collector, no async runtime, no thread pool: the engine loop is
synchronous, the TUI renders on the main thread, and the one worker thread is a
plain `std::thread`. Domain entities use owned types (`String`, `PathBuf`,
`Box<[T]>`) so no lifetimes propagate through use-cases; session checkpoints
move the transcript in and out of the session rather than cloning it; and both
TUI panes are bounded so runaway tool output cannot grow the process.

## Releasing

Version is automatic — read from `[workspace.package].version` in
`Cargo.toml`, not something you pass on the command line:

```sh
./scripts/release.sh bump minor          # or patch / major / an explicit X.Y.Z
./scripts/release.sh bump minor --push   # bumps Cargo.toml, rolls CHANGELOG.md, commits, pushes

# once that commit is on main:
./scripts/release.sh tag --push          # tags vX.Y.Z — X.Y.Z read back from Cargo.toml — and pushes it
```

Pushing the tag triggers `.github/workflows/release.yml`, which builds
`zcode` for Linux x86_64, macOS x86_64/aarch64 and Windows x86_64, and
attaches the packaged archives plus `.sha256` checksums to the GitHub release
for that tag — that's what makes each release downloadable as a binary rather
than something users have to `cargo build` themselves.

To build the same archives locally, with no tag push and no CI wait — version
still read automatically from `Cargo.toml`, override only with
`ZCODE_RELEASE_VERSION` if you need a different label:

```sh
./scripts/package-release.sh                      # mac (both arches) + linux/ubuntu x86_64
./scripts/package-release.sh aarch64-apple-darwin  # or name specific target(s)
```

A target that isn't native to your machine (e.g. Linux from a Mac) builds via
[`cross`](https://github.com/cross-rs/cross) (Docker) if it's on `PATH`, and
is skipped with a note otherwise — plain `cargo`, no local Docker dependency,
still gets you every target native to the host you're on.

Once installed, `zcode --version`/`-V` (or `zcode version` for the build
profile and timestamp too) prints the running binary's version and commit, so
you can check it against a release's tag without guessing.

## Documentation

- **[User guide](docs/guide/README.md)** — installation through to MCP, LSP and telemetry
- [Configuration reference](docs/guide/12-configuration-reference.md) — every key, including several providers at once
- [Command reference](docs/guide/14-commands.md) — every flag, slash command and key
- [Troubleshooting](docs/guide/13-troubleshooting.md)
- [Architecture](docs/architecture/README.md)

## License

Dual-licensed under MIT or Apache-2.0.
