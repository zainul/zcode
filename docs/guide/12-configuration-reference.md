# 12. Configuration reference

← [JSON output & telemetry](11-json-and-telemetry.md) · [Index](README.md) · Next: [Troubleshooting](13-troubleshooting.md)

Every key, its default, and its environment override.

## File locations

| Layer | Path | Purpose |
|-------|------|---------|
| User | `~/.config/zcode/config.json` or `.toml` (or `$XDG_CONFIG_HOME/zcode/…`) | Machine-wide defaults — provider, key variable |
| Project | `zcode.json` or `zcode.toml`, searched **upward** from the current directory | Per-project settings |
| Explicit | `--config <FILE>` | Replaces the project layer |

Precedence, lowest to highest, merged **field by field**:

```
defaults → user config → project config (or --config) → ZCODE_* env → CLI flags
```

`zcode.json` beats `zcode.toml` in the same directory; the nearest project
config beats one further up. `working_dir` defaults to the directory holding
the project config, so `.zcode/` and relative paths are stable no matter which
subdirectory you run from.

Run `zcode config` to see which files were read and what they resolve to.

## Keys

| Key | Type | Default | Env override | Meaning |
|-----|------|---------|--------------|---------|
| `provider` | string | `openai` | `ZCODE_PROVIDER` | `openai`, `anthropic`, `openrouter`, `deepseek`, `ollama`, `vllm`, `openai-compatible` |
| `model` | string | per provider | `ZCODE_MODEL` | Model id as the provider spells it |
| `api_key_env` | string | per provider | `ZCODE_API_KEY_ENV` | **Name** of the variable holding the key — never the key |
| `base_url` | string | per provider | `ZCODE_BASE_URL` | Endpoint override; required for `vllm` and `openai-compatible` |
| `working_dir` | path | directory of the project config | `ZCODE_WORKING_DIR` | Root for file tools and `.zcode/`; `~` expanded |
| `timeout_ms` | integer | `60000` | `ZCODE_TIMEOUT_MS` | HTTP timeout covering a whole streamed response |
| `max_turns` | integer | `20` | `ZCODE_MAX_TURNS` | Hard cap on steps per run |
| `max_tokens` | integer | `16384` | `ZCODE_MAX_TOKENS` | Output-token ceiling sent to the provider |
| `max_tool_output_chars` | integer | `16000` | `ZCODE_MAX_TOOL_OUTPUT_CHARS` | Tool results are trimmed to this before entering the transcript |
| `mode` | string | `build` | `ZCODE_MODE` | `build` or `planning` |
| `shell_allowed` | string[] | `["echo .*","ls .*","cd .*","cat .*"]` | `ZCODE_SHELL_ALLOWED` | **Regexes**, not globs; empty denies everything. `.*` allows anything |
| `skills_dir` | path | *(none)* | `ZCODE_SKILLS_DIR` | **Extra** skills directory; `~` expanded. Searched after `<working_dir>/.zcode/skills` and before `~/.config/zcode/skills` |
| `env` | [string, string][] | `[]` | — | Extra variables placed in the agent context |
| `mcp.servers` | array | `[]` | — | MCP servers (below) |
| `lsp.servers` | array | `[]` | — | Language servers (below) |

`ZCODE_SHELL_ALLOWED` is newline-separated so patterns may contain spaces. Setting
it to the empty string denies every command — a deliberate lockdown, not a
fallback to the defaults.

## Provider defaults

| Provider | Model | Key variable | Endpoint |
|----------|-------|--------------|----------|
| `openrouter` | `openai/gpt-4o-mini` | `ZCODE_OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1/chat/completions` |
| `openai` | `gpt-4o-mini` | `ZCODE_OPENAI_API_KEY` | `https://api.openai.com/v1/chat/completions` |
| `anthropic` | `claude-sonnet-4-5` | `ZCODE_ANTHROPIC_API_KEY` | `https://api.anthropic.com/v1/messages` |
| `deepseek` | `deepseek-chat` | `ZCODE_DEEPSEEK_API_KEY` | `https://api.deepseek.com/chat/completions` |
| `ollama` | `llama3.2` | *(none)* | `http://localhost:11434/api/chat` |
| `vllm` | *(required)* | `ZCODE_API_KEY` | `<base_url>/chat/completions` |
| `openai-compatible` | *(required)* | `ZCODE_API_KEY` | `<base_url>/chat/completions` |

## MCP servers

```json
{
  "mcp": {
    "servers": [
      {
        "name": "everything",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-everything"],
        "env": [["LOG_LEVEL", "warn"]]
      }
    ]
  }
}
```

| Field | Required | Meaning |
|-------|----------|---------|
| `name` | yes | Namespace for its tools: `mcp__<name>__<tool>` |
| `command` | yes | Executable to spawn |
| `args` | no | Arguments |
| `env` | no | Extra environment as `[name, value]` pairs |

## LSP servers

```json
{
  "lsp": {
    "servers": [
      { "language": "rust", "command": "rust-analyzer", "args": [], "env": [] }
    ]
  }
}
```

| Field | Required | Meaning |
|-------|----------|---------|
| `language` | yes | Label used in diagnostics |
| `command` | yes | Executable to spawn |
| `args` | no | Arguments |
| `env` | no | Extra environment |

The first server that starts is used for the whole run.

## Complete example

```json
{
  "provider": "openrouter",
  "model": "anthropic/claude-sonnet-4.5",
  "api_key_env": "ZCODE_OPENROUTER_API_KEY",

  "timeout_ms": 120000,
  "max_turns": 25,
  "max_tokens": 16384,
  "max_tool_output_chars": 16000,
  "mode": "build",

  "shell_allowed": [
    "ls .*",
    "cat .*",
    "git (status|diff|log).*",
    "cargo (build|test|check|fmt|clippy).*"
  ],

  "skills_dir": ".zcode/skills",

  "mcp": {
    "servers": [
      { "name": "everything", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-everything"] }
    ]
  },
  "lsp": {
    "servers": [
      { "language": "rust", "command": "rust-analyzer" }
    ]
  }
}
```

## Files `zcode` writes

| Path | Contents |
|------|----------|
| `.zcode/sessions/<uuid>.json` | Conversation transcripts |
| `.zcode/reports/<ts>-<id>.json` | Per-run telemetry |
| `.zcode/skills/*.md` | Your skill notes (you write these) |

---

Next: [Troubleshooting](13-troubleshooting.md)
