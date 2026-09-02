# 2. Configuration

← [Installation](01-installation.md) · [Index](README.md) · Next: [Your first task](03-first-task.md)

## Where to put the file

`zcode` reads up to two files and layers them, so shared settings live in one
place and each project overrides only what differs.

### 1. Project config — `zcode.json` or `zcode.toml`

Put it at the root of your project, beside `Cargo.toml` / `package.json` /
`.git`:

```
my-project/
├── zcode.json          ← here
├── .zcode/             ← state zcode writes
└── src/
```

It is found by **searching upward** from wherever you run the command, so
`zcode run` works from any subdirectory:

```sh
$ cd my-project/src/deep
$ zcode run "..."        # still uses my-project/zcode.json
```

`zcode.json` wins if both files exist in the same directory, and the **nearest**
config wins if several exist up the tree. Relative paths and the `.zcode/`
state directory are anchored to the directory holding the config, not to the
directory you happened to be in.

### 2. User config — `~/.config/zcode/config.json`

Machine-wide defaults for every project. Exactly one of:

```
~/.config/zcode/config.json      (preferred)
~/.config/zcode/config.toml
```

`$XDG_CONFIG_HOME` is honoured if set, so it may be
`$XDG_CONFIG_HOME/zcode/config.json` instead.

This is the natural home for the things that never change — your provider and
key variable:

```sh
$ mkdir -p ~/.config/zcode
$ cat > ~/.config/zcode/config.json <<'EOF'
{ "provider": "openrouter", "api_key_env": "ZCODE_OPENROUTER_API_KEY" }
EOF
```

Then a project only needs what is specific to it:

```json
{ "model": "anthropic/claude-sonnet-4.5", "shell_allowed": ["cargo (test|build).*"] }
```

### Several providers

The user config is also where a `providers` array belongs — declare every
endpoint once, machine-wide, and let each project pick one:

```json
{
  "provider": "fast",
  "providers": [
    { "name": "fast",  "kind": "openrouter", "model": "anthropic/claude-sonnet-4.5" },
    { "name": "local", "kind": "ollama",     "model": "qwen2.5-coder" }
  ]
}
```

A project then needs one line — `{ "provider": "local" }` — and you can switch
for a single run with `--provider local`, or mid-conversation in the TUI with
`/provider local`. Profiles merge across layers by name, so a project can also
redefine one it disagrees with while keeping the rest.

The full shape is in
[chapter 12](12-configuration-reference.md#multiple-providers).

### 3. Anywhere — `--config <FILE>`

```sh
$ zcode run --config ci/zcode.ci.json "..."
```

Replaces the project layer. The user layer still applies underneath, so a CI
config need not repeat your provider settings.

## Precedence

Lowest to highest — each step overrides the previous one **field by field**, so
setting `model` in a project file does not discard the `provider` from your
user file:

```
built-in defaults
  → ~/.config/zcode/config.json          (user)
  → <project>/zcode.json                  (project, or --config)
  → ZCODE_* environment variables
  → command-line flags (--mode, --timeout, ...)
```

## Checking what is in effect

`zcode config` shows exactly which files were read and what they resolve to —
the fastest way to answer "why is it using that model?":

```sh
$ zcode config
Config sources (later overrides earlier)
  user      /home/you/.config/zcode/config.json
  project   /home/you/my-project/zcode.json

Search paths
  user      /home/you/.config/zcode/config.json  [found]
  user      /home/you/.config/zcode/config.toml  [not found]
  project   zcode.json, then zcode.toml — searched upward from the
            current directory to the filesystem root

Effective configuration
  provider               fast  (openrouter)
  model                  anthropic/claude-sonnet-4.5
  api_key_env            ZCODE_OPENROUTER_API_KEY  [set]
  endpoint               https://openrouter.ai/api/v1/chat/completions (provider default)
  providers              2  (--provider NAME, or /provider NAME in the TUI)
    ▸ fast         openrouter         anthropic/claude-sonnet-4.5
                   https://openrouter.ai/api/v1/chat/completions
      local        ollama             qwen2.5-coder
                   http://localhost:11434/api/chat
  working_dir            /home/you/my-project
  mode                   auto
  timeout_ms             360000
  max_turns              20
  max_tokens             16384
  max_tool_output_chars  16000
  max_retries            3
  rate_limit_backoff_ms  30000ms  (after a 429 with no Retry-After)
  skills_dir             /home/you/my-project/.zcode/skills
  shell_allowed          2 pattern(s)
  shell_denied           23 built-in + 0 from config
  rtk                    0.36.0 — shell output is token-optimised  [/opt/homebrew/bin/rtk]
  mcp servers            0
  lsp servers            0
```

It never prints the key itself — only whether the named variable resolves.

If anything would stop a run, it is listed and the command exits non-zero — so
`zcode config` doubles as a preflight check in CI:

```
Problems
  - invalid shell_allowed pattern "*": error: repetition operator missing expression
      hint: these are regular expressions, not shell globs — use ".*" to allow
            every command (which disables the safety net entirely)
  - ZCODE_OPENROUTER_API_KEY is not set — export it, or point `api_key_env` at
    the variable you use
```

### Paths

`~` is expanded in `working_dir` and `skills_dir`, so
`"skills_dir": "~/.config/zcode/skills"` works as written. (`~user/...` is not
supported.) Relative paths are resolved against `working_dir`.

`skills_dir` **adds** a skills directory; it does not replace the project's
own `.zcode/skills/` or the machine-wide `~/.config/zcode/skills/`. See
[chapter 7](07-tools-and-safety.md#skills).

With no config file anywhere, `zcode` runs on built-in defaults (OpenAI,
`gpt-4o-mini`) and `zcode config` says so.

## Writing a config

The smallest useful config is one line. Everything else has a per-provider
default:

```json
{ "provider": "openrouter" }
```

A fuller one:

```json
{
  "provider": "openrouter",
  "model": "anthropic/claude-sonnet-4.5",
  "api_key_env": "ZCODE_OPENROUTER_API_KEY",

  "timeout_ms": 120000,
  "max_turns": 20,
  "max_tokens": 16384,
  "max_tool_output_chars": 16000,
  "mode": "auto",

  "shell_allowed": [
    "echo( .*)?",
    "ls( .*)?",
    "cat .*",
    "git (status|diff|log)( .*)?",
    "cargo (build|test|check|fmt|clippy)( .*)?"
  ],

  "skills_dir": ".zcode/skills"
}
```

Ready-made examples ship with the source:

```sh
$ cp crates/infra/config/examples/zcode.example.json zcode.json   # JSON
$ cp crates/infra/config/examples/zcode.example.toml zcode.toml   # TOML
```

The equivalent TOML:

```toml
provider = "openrouter"
model = "anthropic/claude-sonnet-4.5"
timeout_ms = 120000
shell_allowed = ["echo( .*)?", "ls( .*)?", "cargo (build|test)( .*)?"]
```

**In TOML, every bare key must come before the first `[table]` header.** Once
you write `[[providers]]` or `[rtk]`, a later `timeout_ms = …` belongs to *that
table*, not to the top level. zcode reports it rather than ignoring it:

```
zcode: toml parse error: TOML parse error at line 5, column 1
  |
5 | timeout_ms = 120000
  | ^^^^^^^^^^
```

## Providing the API key

**Keys are never written to the config file.** The file names an *environment
variable*; `zcode` reads it at startup and holds it in memory only.

```sh
$ export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...
```

Omit `api_key_env` and each provider falls back to its conventional variable:

| `provider` | Default key variable | Default model | Endpoint |
|-----------|----------------------|---------------|----------|
| `openrouter` | `ZCODE_OPENROUTER_API_KEY` | `openai/gpt-4o-mini` | `https://openrouter.ai/api/v1/chat/completions` |
| `openai` | `ZCODE_OPENAI_API_KEY` | `gpt-4o-mini` | `https://api.openai.com/v1/chat/completions` |
| `anthropic` | `ZCODE_ANTHROPIC_API_KEY` | `claude-sonnet-4-5` | `https://api.anthropic.com/v1/messages` |
| `deepseek` | `ZCODE_DEEPSEEK_API_KEY` | `deepseek-chat` | `https://api.deepseek.com/chat/completions` |
| `ollama` | *(none needed)* | `llama3.2` | `http://localhost:11434/api/chat` |
| `vllm` | `ZCODE_API_KEY` | *(you must set `model`)* | your `base_url` |
| `openai-compatible` | `ZCODE_API_KEY` | *(you must set `model`)* | your `base_url` |

Because the defaults track the provider, switching vendors is a one-line
change — the key variable and model follow automatically.

## Verifying

`zcode config` is the direct check — it parses everything and spends no tokens:

```sh
$ zcode config
```

`zcode tools list` is a second check: it builds the whole tool registry
(including MCP and LSP servers) without constructing the LLM client, so it
proves the config parses and the servers start.

A bad config fails immediately and says why:

```sh
$ zcode run "hello"
zcode: configuration error: missing secret env var: ZCODE_OPENROUTER_API_KEY
```

## Local models

No key, no cloud:

```json
{ "provider": "ollama", "model": "qwen2.5-coder:7b" }
```

```json
{
  "provider": "openai-compatible",
  "base_url": "http://localhost:8000/v1",
  "model": "my-served-model"
}
```

`base_url` is required for `vllm` and `openai-compatible`; for the others it is
an optional override (useful for proxies and gateways).

## Environment variables

Every important key has an `ZCODE_*` override, which beats the file. Handy in CI:

```sh
$ ZCODE_MODEL=openai/gpt-4o-mini ZCODE_MODE=planning zcode run "review this module"
```

The full list is in the [configuration reference](12-configuration-reference.md).

Flags beat both. `--model` (or `-m`) takes `<provider>/<model>`, the spelling
opencode and most agent CLIs use — split at the first slash, so the provider
comes first and the rest is the id, slashes and all:

```sh
$ zcode run -m openrouter/z-ai/glm-4.6 "review this module"
$ zcode run -m gpt-4o-mini "review this module"   # no slash: same endpoint
```

Note that the file's `model` key is *not* written that way — it sits next to
`provider`. See [chapter 14](14-commands.md#--model--pick-a-provider-and-model-for-one-run).

## What `zcode` writes to disk

Relative to `working_dir` (the current directory unless you set it):

```
.zcode/
├── sessions/<uuid>.json          transcripts        (chapter 6)
├── reports/<timestamp>-<id>.json run telemetry      (chapter 11)
└── skills/*.md                   your skill notes   (chapter 7)
```

Add `.zcode/` to `.gitignore` unless you intend to share sessions.

---

Next: [Your first task](03-first-task.md)
