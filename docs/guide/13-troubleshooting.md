# 13. Troubleshooting

← [Configuration reference](12-configuration-reference.md) · [Index](README.md)

Errors are one line, never a stack trace. Here is what each one means.

## Configuration

**`configuration error: missing secret env var: ZCODE_OPENROUTER_API_KEY`**

The variable named by `api_key_env` is not exported. Note it is the *name* that
goes in the config, never the key itself.

```sh
$ export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...
$ env | grep ZCODE_          # confirm it is exported, not just set in a subshell
```

**`configuration error: provider `vllm` requires `base_url` ...`**

`vllm` and `openai-compatible` have no default endpoint. Add
`"base_url": "http://localhost:8000/v1"`.

**`unknown provider: gemini`**

Supported values are `openai`, `anthropic`, `openrouter`, `deepseek`, `ollama`,
`vllm`, `openai-compatible`. For anything else with an OpenAI-shaped API, use
`openai-compatible` with a `base_url`.

**`toml parse error` / `json parse error`**

The config file is malformed. Note that `zcode.json` takes precedence over
`zcode.toml` when both exist — you may be editing the file that is not being read.

## Provider errors

**`llm error: openrouter request failed (401): ... — check the API key named by `api_key_env``**

The key was rejected. Confirm it is current and has credit.

**`llm error: ... (404): ... — check `model` and `base_url``**

The model id does not exist on that provider. Ids are provider-specific:
`gpt-4o-mini` on OpenAI is `openai/gpt-4o-mini` on OpenRouter.

**`llm error: ... (429): ... — rate limited or out of credits`**

Retried automatically with backoff (honouring `Retry-After`) before surfacing.
Seeing it means the retries were exhausted.

**`llm error: ... (400): max_tokens is too large ...`**

`max_tokens` defaults to 16384, above some models' output limit. Lower it.

**The run hangs, then times out**

`timeout_ms` covers the whole streamed response, not just the first byte. Raise
it for slow or long-form models:

```json
{ "timeout_ms": 300000 }
```

## Tool problems

**`command blocked by the shell allowlist`**

Working as intended. Add a pattern to `shell_allowed` if the command is one you
want to permit — remembering that patterns are anchored, every `;`/`|` segment
is checked, and `` ` ``, `$(`, `>`, `<`, `&` are refused outright *under a
narrow allowlist*.

```json
{ "shell_allowed": ["cargo (test|build).*", "git status"] }
```

To permit everything the built-in denylist does not refuse — chaining, pipes
and redirection included — set the allowlist open:

```json
{ "shell_allowed": [".*"] }
```

**`hunk 2 does not match src/lib.rs — re-read the file and rebuild the diff`**

The model built a patch against a stale copy. It normally recovers by itself.
If it loops, ask it to read the file first.

**`` `old_str` not found in src/main.rs ``**

Same cause, for `str_replace_editor`. The text must match exactly, whitespace
included.

**`tool `apply_patch` denied in planning mode`**

Planning mode withholds every editing tool. Use `--mode build`.

**`mcp server `x` failed to start: ... No such file or directory`**

The command is not on `PATH`. The agent runs on without it; fix the `command`
in the config or install the server.

**`no language server is configured`**

The model called an `lsp__*` tool but no server started. Check `lsp.servers`
and that the binary (e.g. `rust-analyzer`) is installed.

## Stale installation

**A subcommand that should exist says `unrecognized subcommand`**

You are running an older binary than your source tree. The version number and
commit do not change between releases, so compare the build stamp:

```sh
$ command -v zcode
$ zcode version
zcode v0.2.0 (git: 9a99381, built: 2026-08-26T03:31:35Z, release)
```

Then update in place:

```sh
$ git pull && ./scripts/update.sh
```

If `install.sh` warns that other copies are on your `PATH`, remove them with
`./scripts/uninstall.sh` and install once more — whichever copy comes first in
`PATH` is the one that runs.

**`invalid shell_allowed pattern "*"`**

`shell_allowed` takes regular expressions, not shell globs. Use `.*`, not `*`.
Run `zcode config` to see every problem at once.

## Runtime

**`zcode: Device not configured (os error 6)`**

`zcode repl` needs a real terminal. Use `zcode run` in scripts and CI.

**Exit code 130**

Ctrl-C. The session was checkpointed; resume with `zcode run --session <id> "..."`.

**`truncated: true` in the report**

The run hit `max_turns` or `max_tokens` before finishing. Raise `max_turns`, or
break the task into smaller pieces.

**Token counts look suspiciously round**

The provider omitted its `usage` block and `zcode` fell back to a word-count
estimate. Common with some Ollama builds; cloud providers report real numbers.

## Diagnostics

```sh
# Version and commit — quote these in bug reports
$ zcode version

# Verbose logging, including MCP/LSP startup
$ RUST_LOG=debug zcode run "..." 2>debug.log

# Does the config parse and do the tools load? (no API call, no tokens)
$ zcode tools list

# What did the agent actually do?
$ jq -r '.messages[] | "\(.role): \(.content[:100])"' .zcode/sessions/<id>.json

# What did a run cost?
$ jq . .zcode/reports/<file>.json
```

## Reporting a bug

Include the `zcode version` line, the config with secrets removed, the exact
command, the one-line error, and — if you can share it — the session file from
`.zcode/sessions/`.

## `command refused: it matches zcode's built-in denylist`

Not the allowlist — a separate, always-on list of irreversible commands
(`rm -rf`, `sudo`, `git push --force`, `npm publish`, …). Widening
`shell_allowed` will not help, and the message says so. If you genuinely need
the command, run it yourself.

See [chapter 7](07-tools-and-safety.md#2-the-denylist--which-shell_allowed-cannot-override)
for the full list.

## `command blocked by the shell allowlist`

The command did not match any `shell_allowed` pattern. The error names the
program and suggests a pattern:

```
  hint: no pattern in `shell_allowed` matches `go`; add one, e.g. "go( .*)?"
```

If the hint instead mentions metacharacters, the command contains `$(`, a
backtick, `>`, `<`, or `&&`. Under a narrow allowlist only `2>&1` and
`>/dev/null` are permitted, so either rewrite the command without the rest or
open the allowlist with `"shell_allowed": [".*"]`, which switches the structure
check off as well. The denylist still applies either way.

If `"shell_allowed": [".*"]` is already set and a command with `&&` or a pipe is
*still* refused, you are on a build from before zcode 0.2 — that was a bug: the
structure check ran ahead of the allowlist and so refused commands an open
allowlist had already permitted.

Note that the shipped default already covers Go, Rust, Node, Python, and the
common build tools — if `go build` is blocked, something in your config
*replaced* the default list.

## The agent pauses for many seconds, then continues

It was rate limited. You will see:

```
↻ rate limited by the provider (429) — retrying in 2.0s (attempt 1/3)
```

zcode honours the provider's `Retry-After` header when it sends one. Without
one, a 429 waits a flat `rate_limit_backoff_ms` (30s) — the metering window is
fixed, so doubling would only lengthen the silence — while a transient 5xx
starts at 500ms and doubles. Both carry jitter and are capped at 120s.
`max_retries` (default 3) sets how many times, so the worst case for a rate
limit is 90 seconds. Raising it trades latency for a better chance of
completing; lowering it fails faster.

## The cost shows `n/a`

There is no rate in the price table for your model. Add one under `pricing` —
see [chapter 12](12-configuration-reference.md#cost-estimates). `n/a` means
"unknown", never "free"; a model known to be free shows `$0.00`.

## No language server started

`zcode config` tells you what it looked for:

```
  lsp servers            0  (project looks like go)
    (none — no language server for this project is installed)
```

Install the server for the language it detected. If it detected nothing, there
is no `go.mod`, `Cargo.toml`, `tsconfig.json`, or `package.json` at
`working_dir` — check that `working_dir` points where you expect.

## `openai-compatible request failed … /chat/completions/chat/completions`

Older versions appended `/chat/completions` to `base_url` unconditionally, so a
full endpoint became a doubled path. Current versions accept either spelling.
Run `zcode version` — if it predates this fix, update with
`./scripts/update.sh`.

## A `/command` was sent to the model instead of running

Only the commands in `/help` are recognised. Anything else — including a path
like `/usr/local/bin` — is treated as a prompt, deliberately. A near-miss is
caught:

```
unknown command `/exitt` — /help lists them all
```


---

[Back to the index](README.md)
