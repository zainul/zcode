# zcode — User Guide

A step-by-step walkthrough of every feature, from a fresh clone to MCP servers
and telemetry. Follow the chapters in order the first time; after that, use it
as a reference.

## Chapters

| # | Chapter | What you get |
|---|---------|--------------|
| 1 | [Installation](01-installation.md) | Install and uninstall scripts; a working `zcode` on your `PATH` |
| 2 | [Configuration](02-configuration.md) | Where the config file goes, one provider or several, API keys |
| 3 | [Your first task](03-first-task.md) | A real edit made by the agent |
| 4 | [Headless CLI](04-headless-cli.md) | `zcode run` in depth: flags, streaming, exit codes |
| 5 | [Interactive TUI](05-tui.md) | Conversational multi-step work; scrolling, selecting, switching provider |
| 6 | [Sessions](06-sessions.md) | Create, continue, fork, import, export |
| 7 | [Tools & safety](07-tools-and-safety.md) | File edits, patches, the shell allowlist, rtk, skills |
| 8 | [Agent modes](08-agent-modes.md) | Planning, editing, and auto |
| 9 | [MCP & LSP](09-mcp-and-lsp.md) | External data sources and semantic code intel |
| 10 | [Multimodal input](10-multimodal.md) | Sending images to vision models |
| 11 | [JSON output & telemetry](11-json-and-telemetry.md) | JSONL events, token/cost accounting, CI |
| 12 | [Configuration reference](12-configuration-reference.md) | Every key and environment variable |
| 13 | [Troubleshooting](13-troubleshooting.md) | What each error means and how to fix it |
| 14 | [Command reference](14-commands.md) | Every CLI flag, slash command, key, and environment variable |
| 15 | [Event reference](15-events.md) | The `--json` schema, and how it relates to opencode's |

## Five-minute quick start

```sh
# 1. Build and install — detects your platform, puts zcode on your PATH
git clone <this-repo> && cd zcode
./scripts/install.sh

# 2. Configure — this is a complete config
echo '{ "provider": "openrouter", "model": "anthropic/claude-sonnet-4.5" }' > zcode.json

# 3. Authenticate (keys are read from the environment, never stored in the file)
export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...

# 4. Run a task
zcode run "add doc comments to the public functions in src/main.rs"
```

Removing it later is `./scripts/uninstall.sh`. Both scripts are covered in
[chapter 1](01-installation.md).

## What `zcode` is

A terminal coding agent: you describe a task in plain language, it reads and
edits files, runs allowlisted shell commands, and can reach external systems
through MCP servers and language servers. It talks to OpenRouter, OpenAI,
Anthropic, DeepSeek, Ollama, vLLM, or any OpenAI-compatible endpoint.

Language servers are on by default: drop into a Go, Rust, or TypeScript/Next.js
project and zcode starts the right one if you have it installed, with no
configuration.

It is written in Rust with no garbage collector and no async runtime: the
release binary is under 5 MB, starts in a few milliseconds, and a full agent
run holds roughly 5 MB of resident memory.

## Conventions in this guide

- `$` marks a command you type; everything below it is real output.
- Anything in `<angle brackets>` is a value you substitute.
- Model-generated text varies between runs — the *shape* of the output is what
  matters, not the exact words.
