# 3. Your first task

← [Configuration](02-configuration.md) · [Index](README.md) · Next: [Headless CLI](04-headless-cli.md)

Let us give the agent a real edit to make.

## Step 1 — A small project to work on

```sh
$ mkdir -p ~/zcode-demo/src && cd ~/zcode-demo
$ cat > src/main.rs <<'EOF'
fn main() {
    println!("{}", greet("world"));
}

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
EOF
$ echo '{ "provider": "openrouter", "model": "anthropic/claude-sonnet-4.5" }' > zcode.json
$ export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...
```

## Step 2 — Ask for the change

```sh
$ zcode run "add a farewell function to src/main.rs, mirroring greet"
```

Output arrives as it is generated:

```
I'll add the function.
· apply_patch
  apply_patch: patched src/main.rs (1 hunk(s))
Added `farewell` to src/main.rs.

[2 step(s) · 2490 in / 110 out / 2204 cached tokens · session 01a03bd4-8313-7b32-9809-7d9984359dda]
```

Reading that:

- Plain text is the model talking.
- `·` marks a **tool call** — here the agent chose `apply_patch`.
- The indented line is the **tool result**.
- The bracketed summary goes to **stderr**, so `zcode run ... > out.txt` captures
  only the model's answer.

The file really changed:

```sh
$ cat src/main.rs
fn main() {
    println!("{}", greet("world"));
}

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

/// Returns a farewell for `name`.
fn farewell(name: &str) -> String {
    format!("Goodbye, {}!", name)
}
```

## What just happened

```
your prompt
     │
     ▼
┌─────────────────────────────────────────────┐
│  1. send history + tool definitions to LLM  │◀───┐
│  2. stream the reply token by token         │    │
│  3. reply asks for a tool?                  │    │ up to
│       yes → run it, append the result ──────┼────┘ max_turns
│       no  → this is the final answer        │
│  4. checkpoint the session, record telemetry│
└─────────────────────────────────────────────┘
```

Each pass through that loop is one **step**. The run above took two: one to
call `apply_patch`, one to report back. Two limits keep it bounded —
`max_turns` (default 20) and `max_tokens` (default 16384).

## Step 3 — Ask a follow-up

Each `zcode run` starts a fresh session by default. To continue the previous one,
pass the id from the summary line:

```sh
$ zcode run --session 01a03bd4-8313-7b32-9809-7d9984359dda "now add a test for it"
```

Chapter 6 covers sessions properly.

## Step 4 — Look without touching

Planning mode withholds every editing tool, so the agent can only read and
advise:

```sh
$ zcode run --mode planning "how would you restructure this file?"
```

## If something went wrong

| Message | Meaning |
|---------|---------|
| `configuration error: missing secret env var: ...` | The key variable is not exported |
| `llm error: ... (401) ... check the API key` | Key rejected by the provider |
| `llm error: ... (404) ... check model and base_url` | Model id not available on that provider |
| `command blocked by the shell allowlist` | Working as designed — see [chapter 7](07-tools-and-safety.md) |

More in [troubleshooting](13-troubleshooting.md).

---

Next: [Headless CLI](04-headless-cli.md)
