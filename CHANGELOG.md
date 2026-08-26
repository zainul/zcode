# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed — renamed to `zcode`

The project, binary, and every user-facing identifier moved from `ag` to
`zcode`. This is a breaking change for existing installs:

| Was | Now |
|-----|-----|
| `ag` binary and cargo package | `zcode` |
| `ag.json` / `ag.toml` | `zcode.json` / `zcode.toml` |
| `AG_*` environment variables | `ZCODE_*` (e.g. `ZCODE_OPENROUTER_API_KEY`) |
| `.ag/` state directory | `.zcode/` |
| `ag_skill` tool (alias `ag:skill`) | `zcode_skill` (alias `zcode:skill`) |
| `ag-benches` crate | `zcode-benches` |

To migrate an existing project: `mv ag.json zcode.json`, `mv .ag .zcode`, and
re-export your key under the new variable name.

Outbound identity changed with it: the HTTP `User-Agent` is now
`zcode/<version>`, and the MCP/LSP `clientInfo.name` and OpenRouter `X-Title`
report `zcode`.

### Added

- **Skills are discoverable by the model.** The `zcode_skill` tool description
  now lists every available skill with a one-line summary, so the agent can
  choose one unprompted. Previously it was told only "load a skill from the
  skills directory" and had to guess a filename, so in practice skills were
  never used.
- **`<name>/SKILL.md` layout supported** alongside `<name>.md`. A library using
  the Agent Skills convention was previously invisible: only top-level `*.md`
  was scanned.
- **Skills are searched across several roots** — the project's
  `.zcode/skills/`, any configured `skills_dir`, and the machine-wide
  `~/.config/zcode/skills/`. `skills_dir` now *adds* a root instead of
  replacing the project's, which had made per-project skills impossible
  whenever a global library was configured. Nearer roots shadow further ones.
- Skill summaries come from YAML front-matter `description:` when present,
  otherwise the first line of prose.
- `zcode skills list` shows the directories searched, each skill with its
  summary, and — when nothing is found — how to create one.

### Changed

- `zcode_skill` is no longer gated by planning mode. Loading a markdown note is
  read-only, and planning is exactly when house conventions should inform the
  proposal.
- The skill tool is not registered at all when no skills exist, so it costs no
  prompt budget and cannot invite a guessed name.
- An unknown skill name lists the available ones instead of failing opaquely.

### Added

- `scripts/update.sh`, and `install.sh` is now update-aware: it replaces an
  existing installation **in place** rather than installing a second copy to a
  different default prefix, prints the old and new build stamps, and warns when
  another copy earlier on `PATH` would shadow the one just installed.
- `zcode version` includes a build timestamp and marks a dirty working tree
  (`git: 9a99381-dirty, built: 2026-08-26T03:31:35Z`). Between releases the
  crate version and commit are identical, so without it there was no way to
  tell a stale installed binary from a fresh build — the cause of
  "`zcode config` says unrecognized subcommand".
- `zcode config` reports **problems** as well as values — an invalid
  `shell_allowed` regex, a missing API key variable, a `skills_dir` that does
  not exist — and exits non-zero, so it works as a CI preflight check.
- `~` is expanded in `working_dir` and `skills_dir`. A config saying
  `"skills_dir": "~/.config/zcode/skills"` previously resolved to a literal
  `./~/...` that never existed.
- An invalid `shell_allowed` pattern now reports the regex error and a hint.
  `"*"` (shell-glob thinking) failed every run with only
  `invalid shell_allowed pattern: *`; it now explains that these are regular
  expressions and suggests `.*`.
- **Layered configuration discovery.** A user-level
  `~/.config/zcode/config.{json,toml}` (honouring `$XDG_CONFIG_HOME`) now sits
  under the project config, so provider and key settings can be set once per
  machine. The project file is found by walking **up** from the current
  directory rather than only checking it, and `working_dir` anchors to the
  directory holding it — previously, running from a subdirectory silently fell
  back to built-in defaults with no warning.
- `zcode config` — prints which files were read, the full search path, and the
  effective settings, reporting only whether the API key variable resolves and
  never its value.
- `scripts/install.sh` — detects platform and architecture, builds the release
  binary, installs to `/usr/local/bin` or `~/.local/bin`, and prints the exact
  `PATH` line for the user's shell. Supports `--prefix`, `--no-build`,
  `ZCODE_INSTALL_DIR`. POSIX `sh`, so it runs under bash/zsh/dash/ash and in
  Git Bash, MSYS2 and WSL.
- `scripts/uninstall.sh` — finds every install on `PATH` plus the usual
  locations, confirms before deleting, and refuses to run unattended without
  `--yes`. Project data (`.zcode/`, `zcode.json`) is deliberately left alone
  and its location reported.
- `docs/guide/` — a 14-chapter user guide covering installation through MCP,
  LSP, multimodal input, and telemetry.

## [0.2.0] - 2026-08-24

### Added — core capability milestone

- **Agent loop** (`app::AgentLoop::execute`): streams the model, dispatches
  tool calls, feeds results back, checkpoints each round, and stops on the
  turn/token caps (FR-LOOP-01..04).
- **Two interfaces on one engine**: headless `zcode run` (with `--json` JSONL) and
  an interactive ratatui TUI (`zcode`/`zcode repl`) (FR-IFACE-01..06).
- **Tool registry** (`crates/tools`): native `read`, `write`,
  `str_replace_editor`, `list_dir`, `shell`, `zcode_skill`, merged with MCP and
  LSP tools into one namespace (FR-TOOL-*, DQ10).
- **Shell allowlist**: `GuardedShell` checks every command segment against the
  configured regexes before execution, and refuses substitution/redirection
  outright; an empty list denies everything (FR-CONFIG-04/05, NFR-SEC-02).
- **MCP client** (`crates/infra/mcp`): stdio JSON-RPC with deadlines; servers
  that fail to start are logged and skipped (FR-MCP-01..05).
- **LSP client** (`crates/infra/lsp`): stdio JSON-RPC with `Content-Length`
  framing, document mirroring, and `didChange` after edits (FR-LSP-01..04).
- **Session subcommands**: `create`, `continue`, `fork`, `import`, `export`
  (FR-SESSION-01..05).
- **Agent modes**: planning mode withholds every editing tool from the model
  and refuses one if requested anyway (FR-MODE-01..04).
- **Provider dispatch** in `wire()` for OpenAI, Anthropic, OpenRouter, Ollama,
  and vLLM/OpenAI-compatible endpoints (FR-MODEL-01..06).
- **Vision input**: `--image` files are base64-encoded into the request
  (FR-MODEL-08).
- `domain::canonical_tool_name`: `mcp::srv::tool` / `zcode:skill` map to
  provider-legal `mcp__srv__tool` / `zcode_skill`, with the `::` spellings kept as
  aliases so dispatch and mode-gating can never disagree.
- `make check-arch` and `make secrets-scan`.

- **Real incremental streaming**: response bodies are decoded line by line as
  they arrive, so the first token appears immediately instead of after the
  whole generation. Decoding lives in `SseDecode` implementations shared by the
  live reader and the batch parsers, so tests exercise the production path.
- **`apply_patch` tool**: unified diffs across multiple files, including
  creation and deletion. Hunks are located by context rather than by trusting
  `@@` line numbers, and nothing is written unless every hunk applies.
- **DeepSeek provider**, plus per-provider defaults for `model` and
  `api_key_env` so switching provider is a one-line change.
- **JSON configuration** (`zcode.json`), preferred over `zcode.toml` when both exist.
- HTTP hardening: retries with exponential backoff and `Retry-After` support
  for 429/5xx, immediate failure with the provider's own message for auth and
  model errors, and OpenRouter attribution headers.
- `timeout_ms` now reaches the provider clients instead of a hardcoded 30s.

### Changed

- `zcode session fork --as <name>` accepts any short, filesystem-safe name
  (letters, digits, `-`, `_`, `.`; max 64 chars). It previously demanded a
  UUIDv7, which no one can produce by hand, so the flag was unusable.
  Generated ids are still UUIDv7; traversal (`..`, separators, leading dots)
  is still refused.
- `mcp` and `lsp` are cargo features of the `zcode` binary, forwarded to `tools`,
  so `--no-default-features` genuinely drops those adapters. Previously the
  flag was silently ineffective.
- Tool results report paths relative to the working directory instead of
  repeating a long absolute prefix on every line.
- `--help` and `--version` exit 0 and print to stdout instead of being routed
  through the error handler with an `ag:` prefix and exit 1. Usage errors now
  exit 2.
- Help text no longer leaks internal requirement ids (`FR-IFACE-01`) into the
  product's `--help` output.

### Fixed

- Anthropic conversations with tool calls were malformed: the system prompt was
  sent as a user message, `tool_use` blocks were not emitted on assistant
  turns, and tool results used an invalid shape. Multi-turn tool use against
  Anthropic could not have worked.
- Anthropic cache accounting picked one usage field where writes and reads are
  reported separately; they are now summed.
- Ollama tool calls whose `arguments` are a JSON object (its actual wire shape,
  not OpenAI's string) were dropped.
- OpenAI streams `usage` in a trailing chunk *after* `finish_reason`; those
  token counts were dropped, so every run reported zero and silently fell back
  to the estimate heuristic. They are now folded into the finish event.
- The CLI ran its synchronous engine inside a `#[tokio::main]` runtime, which
  made `reqwest::blocking` panic on drop ("Cannot drop a runtime in a context
  where blocking is not allowed") — every real LLM call aborted. The runtime is
  gone; nothing in the agent is async.
- `UuidSessionStore::checkpoint` now stamps `last_message_at`.

### Changed

- `App` owns its ports as `Box<dyn Port + Send>` and exposes `AgentLoop`
  instead of the v0.1 `TaskRunner`/`EditPlanner` stubs (DQ5).
- `crates/cli` no longer depends on `tokio` or on `crossterm` directly
  (crossterm comes through ratatui's re-export, so the versions cannot skew).

### Added

- Workspace scaffolding for the zcode Rust coding agent.
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
