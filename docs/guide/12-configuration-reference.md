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
| `provider` | string | `openai` | `ZCODE_PROVIDER` | Which endpoint to use: a name from `providers`, or a built-in kind (`openai`, `anthropic`, `openrouter`, `deepseek`, `ollama`, `vllm`, `openai-compatible`) |
| `providers` | array | `[]` | — | Named endpoints to switch between (below) |
| `model` | string | per provider | `ZCODE_MODEL` | Model id as the provider spells it |
| `api_key_env` | string | per provider | `ZCODE_API_KEY_ENV` | **Name** of the variable holding the key — never the key |
| `base_url` | string | per provider | `ZCODE_BASE_URL` | Endpoint override; required for `vllm` and `openai-compatible` |
| `working_dir` | path | directory of the project config | `ZCODE_WORKING_DIR` | Root for file tools and `.zcode/`; `~` expanded |
| `timeout_ms` | integer | `60000` | `ZCODE_TIMEOUT_MS` | HTTP timeout covering a whole streamed response |
| `max_turns` | integer | `20` | `ZCODE_MAX_TURNS` | Hard cap on steps per run |
| `max_tokens` | integer | `16384` | `ZCODE_MAX_TOKENS` | Output-token ceiling sent to the provider |
| `max_tool_output_chars` | integer | `16000` | `ZCODE_MAX_TOOL_OUTPUT_CHARS` | Tool results are trimmed to this before entering the transcript |
| `max_retries` | integer | `3` | `ZCODE_MAX_RETRIES` | Retries for a 429 or transient 5xx before the run fails |
| `rate_limit_backoff_ms` | integer | `30000` | `ZCODE_RATE_LIMIT_BACKOFF_MS` | Flat wait after a 429 that carries no `Retry-After`. Worst case is `max_retries` × this |
| `mode` | string | `auto` | `ZCODE_MODE` | `planning`, `editing`, or `auto` (`build` is accepted as `auto`) |
| `shell_allowed` | string[] | a working dev toolchain — see [chapter 7](07-tools-and-safety.md#3-the-allowlist) | `ZCODE_SHELL_ALLOWED` | **Regexes**, not globs; empty denies everything. `".*"` allows anything the denylist permits, `&&`/pipes/redirection included |
| `shell_denied` | string[] | `[]` | `ZCODE_SHELL_DENIED` | Extra always-on deny regexes. These **add** to the built-in denylist; nothing can remove a built-in |
| `skills_dir` | path | *(none)* | `ZCODE_SKILLS_DIR` | **Extra** skills directory; `~` expanded. Searched after `<working_dir>/.zcode/skills` and before `~/.config/zcode/skills` |
| `env` | [string, string][] | `[]` | — | Extra variables placed in the agent context |
| `mcp.servers` | array | `[]` | — | MCP servers (below) |
| `lsp.servers` | array | *(auto-detected)* | — | Language servers (below); defaults are chosen from the project's own files |
| `lsp.defaults` | bool | `true` | — | Set `false` to start no language server unless one is listed |
| `pricing` | array | `[]` | — | Per-model rate overrides for the cost estimate (below) |

`ZCODE_SHELL_ALLOWED` and `ZCODE_SHELL_DENIED` are newline-separated so patterns
may contain spaces. Setting `ZCODE_SHELL_ALLOWED` to the empty string denies
every command — a deliberate lockdown, not a fallback to the defaults.

Two keys accumulate across layers rather than being replaced: `shell_denied`
(so a machine-wide ban cannot be dropped by a project file) and `pricing` (with
the nearer layer taking precedence).

## Retries and rate limits

A rate limit is not a transient hiccup. A provider that just refused you is
refusing everyone, and free or shared tiers meter by the minute — so coming
back in 600ms only spends another request to be told the same thing.

zcode therefore backs off differently depending on *why* it is retrying:

| Cause | Wait | Then |
|-------|------|------|
| 429 with a `Retry-After` header | exactly what the provider asked | — |
| 429 without one | `rate_limit_backoff_ms` (**30s**) | the same again |
| 5xx, timeout, dropped connection | 500ms | doubles each attempt |

A rate limit does **not** back off exponentially. The window a provider meters
over is fixed — usually a minute — so 30s, then 60s, then 120s does not improve
the odds; it just turns a recoverable pause into minutes of silence. A flat
wait keeps the worst case predictable: `max_retries × rate_limit_backoff_ms`,
90 seconds at the defaults. A transient 5xx *is* worth backing off from
progressively, so it still does.

Every wait carries jitter (so two agents throttled at the same instant do not
return at the same instant) and is capped at 120 seconds, so a hostile or
mistaken `Retry-After` cannot park the agent.

On a heavily throttled free tier, the knob worth turning is the wait, not the
count:

```json
{ "max_retries": 3, "rate_limit_backoff_ms": 45000 }
```

## Multiple providers

`providers` is an array of named endpoints. `provider` says which one is
active; `--provider NAME` overrides it for one run, and `/provider NAME`
switches mid-session in the TUI without losing the conversation.

```json
{
  "provider": "free",
  "providers": [
    {
      "name": "free",
      "kind": "openrouter",
      "model": "poolside/laguna-s-2.1:free"
    },
    {
      "name": "fast",
      "kind": "openrouter",
      "model": "anthropic/claude-haiku-4.5",
      "api_key_env": "WORK_OPENROUTER_KEY"
    },
    {
      "name": "gateway",
      "kind": "openai-compatible",
      "model": "internal-1",
      "base_url": "https://gateway.internal/v1/chat/completions"
    },
    { "name": "local", "kind": "ollama" }
  ]
}
```

In TOML the same thing is `[[providers]]` tables.

| Field | Meaning |
|-------|---------|
| `name` | How it is selected. Defaults to `kind` |
| `kind` | Which protocol it speaks — one of the built-in providers. Defaults to `name`. (`provider` is accepted as a spelling of `kind`) |
| `model` | Model id. Defaults to the kind's default |
| `api_key_env` | **Name** of the variable holding the key. Defaults to the kind's |
| `base_url` | Endpoint. Defaults to the kind's |

One of `name` and `kind` must be given; an entry with neither cannot say what
it talks to and is an error rather than a silent default.

Two rules are worth knowing:

1. **Naming a profile after a built-in kind overrides that kind.** This is how
   you give OpenRouter a different URL:

   ```json
   {
     "provider": "openrouter",
     "providers": [
       { "name": "openrouter", "base_url": "https://gateway.internal/v1/chat/completions" }
     ]
   }
   ```

2. **A declared profile is complete in itself.** What it does not state comes
   from the defaults for its `kind` — never from a top-level `model`,
   `api_key_env`, or `base_url`. Those were written for whichever provider the
   config had at the time, and letting an unrelated gateway inherit a key
   variable produces one that reads `[set]` in `zcode config` and then fails on
   the first request. They still apply to a provider selected as a bare kind
   with no profile behind it, which is every config written before `providers`
   existed.

`zcode config` prints the whole list with the active one marked, so which
endpoint you are pointed at is never a guess:

```
provider               free  (openrouter)
model                  poolside/laguna-s-2.1:free
api_key_env            ZCODE_OPENROUTER_API_KEY  [set]
endpoint               https://openrouter.ai/api/v1/chat/completions (provider default)
providers              4  (--provider NAME, or /provider NAME in the TUI)
  ▸ free         openrouter         poolside/laguna-s-2.1:free
                 https://openrouter.ai/api/v1/chat/completions
    gateway      openai-compatible  internal-1
                 https://gateway.internal/v1/chat/completions
```

There is no `ZCODE_PROVIDERS`: an array of endpoints is a thing you write down,
not a thing you export. `ZCODE_PROVIDER` still selects one by name.

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

## Cost estimates

zcode ships a table of published list prices so the TUI can show a running cost
without configuration. It is an **estimate, not a bill** — providers change
prices, negotiate discounts, and meter cache reads differently. Reconcile
anything that matters against your provider's dashboard.

Model ids are matched by longest prefix, ignoring the vendor namespace and any
routing suffix, and treating `.` and `-` alike — so `openai/gpt-4o-mini`,
`gpt-4o-mini`, and `anthropic/claude-3.5-haiku` all resolve. An OpenRouter
`:free` route is treated as free. An unknown model shows `n/a` rather than a
confident `$0.00`.

Override or add a rate:

```json
{
  "pricing": [
    {
      "model": "acme-1",
      "input_per_mtok": 1.5,
      "output_per_mtok": 6.0,
      "cache_per_mtok": 0.15,
      "cache_within_input": true
    }
  ]
}
```

| Field | Meaning |
|-------|---------|
| `model` | Matched as a prefix of the model id |
| `input_per_mtok` | USD per million input tokens |
| `output_per_mtok` | USD per million output tokens |
| `cache_per_mtok` | USD per million cached tokens |
| `cache_within_input` | `true` (default) when the provider's `input_tokens` already includes the cached prefix, as OpenAI's does; `false` for Anthropic, which reports them separately |

`cache_within_input` exists so the estimate is not double-counted: with `true`,
cached tokens are subtracted from billable input before the input rate applies.

`zcode config` shows which rate matched:

```
  pricing                $1/$5 per Mtok in/out (matched `claude-haiku-4`)
```

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

Every key, with several providers. Nothing here is required — each line falls
back to the default in the table above if you delete it.

```json
{
  "provider": "fast",

  "providers": [
    {
      "name": "fast",
      "kind": "openrouter",
      "model": "anthropic/claude-sonnet-4.5",
      "api_key_env": "ZCODE_OPENROUTER_API_KEY"
    },
    {
      "name": "free",
      "kind": "openrouter",
      "model": "poolside/laguna-s-2.1:free",
      "api_key_env": "ZCODE_OPENROUTER_API_KEY"
    },
    {
      "name": "gateway",
      "kind": "openai-compatible",
      "model": "internal-1",
      "base_url": "https://gateway.internal/v1/chat/completions",
      "api_key_env": "WORK_GATEWAY_TOKEN"
    },
    {
      "name": "local",
      "kind": "ollama",
      "model": "qwen2.5-coder",
      "base_url": "http://127.0.0.1:11434/api/chat"
    }
  ],

  "working_dir": "~/src/my-project",
  "mode": "auto",

  "timeout_ms": 120000,
  "max_turns": 25,
  "max_tokens": 16384,
  "max_tool_output_chars": 16000,

  "max_retries": 3,
  "rate_limit_backoff_ms": 30000,

  "shell_allowed": [
    "ls( .*)?",
    "cat .*",
    "git (status|diff|log)( .*)?",
    "cargo (build|test|check|fmt|clippy)( .*)?"
  ],
  "shell_denied": [
    "\\bterraform apply\\b",
    "\\bkubectl delete\\b"
  ],

  "skills_dir": "~/notes/zcode-skills",

  "env": [
    ["RUST_BACKTRACE", "1"],
    ["CI", "false"]
  ],

  "pricing": [
    {
      "model": "internal-1",
      "input_per_mtok": 1.5,
      "output_per_mtok": 6.0,
      "cache_per_mtok": 0.15,
      "cache_within_input": true
    }
  ],

  "mcp": {
    "servers": [
      {
        "name": "everything",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-everything"],
        "env": [["LOG_LEVEL", "warn"]]
      }
    ]
  },

  "lsp": {
    "defaults": true,
    "servers": [
      { "language": "rust", "command": "rust-analyzer", "args": [], "env": [] }
    ]
  }
}
```

Start on `fast`, drop to `free` when you are iterating, `local` on a plane:

```sh
zcode --provider free
zcode run --provider local "summarise this diff"
```

…or, in the TUI, `/provider local` — which keeps the conversation.

### The same thing in TOML

`providers`, `mcp.servers`, `lsp.servers` and `pricing` become arrays of
tables. Everything else is a plain key:

```toml
provider = "fast"
mode = "auto"
max_turns = 25
max_retries = 3
rate_limit_backoff_ms = 30000
shell_allowed = ["ls( .*)?", "cargo (build|test)( .*)?"]
shell_denied = ['\bterraform apply\b']

[[providers]]
name = "fast"
kind = "openrouter"
model = "anthropic/claude-sonnet-4.5"

[[providers]]
name = "gateway"
kind = "openai-compatible"
model = "internal-1"
base_url = "https://gateway.internal/v1/chat/completions"
api_key_env = "WORK_GATEWAY_TOKEN"

[[pricing]]
model = "internal-1"
input_per_mtok = 1.5
output_per_mtok = 6.0

[[mcp.servers]]
name = "everything"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]

[lsp]
defaults = true

[[lsp.servers]]
language = "rust"
command = "rust-analyzer"
```

### Keeping it small

The example above is exhaustive on purpose. A real project file is usually four
lines, because everything else has a working default:

```json
{
  "provider": "openrouter",
  "model": "anthropic/claude-sonnet-4.5"
}
```

Declare the providers once in `~/.config/zcode/config.json`:

```json
{
  "provider": "fast",
  "providers": [
    { "name": "fast",  "kind": "openrouter", "model": "anthropic/claude-sonnet-4.5" },
    { "name": "local", "kind": "ollama",     "model": "qwen2.5-coder" }
  ]
}
```

…and each project only has to name the one it wants:

```json
{ "provider": "local" }
```

Profiles merge across layers **by name**, so a project can also redefine one it
disagrees with while keeping the rest:

```json
{
  "provider": "local",
  "providers": [
    { "name": "local", "kind": "ollama", "model": "llama3.2",
      "base_url": "http://127.0.0.1:9999/api/chat" }
  ]
}
```

`fast` is still there, unchanged, and `zcode config` shows both.

## Files `zcode` writes

| Path | Contents |
|------|----------|
| `.zcode/sessions/<uuid>.json` | Conversation transcripts |
| `.zcode/reports/<ts>-<id>.json` | Per-run telemetry |
| `.zcode/skills/*.md` | Your skill notes (you write these) |

---

Next: [Troubleshooting](13-troubleshooting.md)
