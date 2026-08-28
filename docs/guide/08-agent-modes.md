# 8. Agent modes

← [Tools & safety](07-tools-and-safety.md) · [Index](README.md) · Next: [MCP & LSP](09-mcp-and-lsp.md)

Three modes swap both the system prompt and the available tool set. They form a
ladder, each a strict superset of the one before:

| | `planning` | `editing` | `auto` (default) |
|---|---|---|---|
| Read files, search, LSP queries | yes | yes | yes |
| Load skills, call MCP read tools | yes | yes | yes |
| `write`, `str_replace_editor`, `apply_patch`, `lsp__rename_symbol` | **withheld** | yes | yes |
| `shell` | **withheld** | **withheld** | yes |
| System prompt | propose changes, ask for confirmation | edit directly, do not run commands | act autonomously: edit, then verify |
| Typical use | "what would you change?" | "make this change, I'll run the tests" | "make it work" |

`editing` exists because *"may rewrite my source"* and *"may execute arbitrary
commands"* are genuinely different grants of trust. An agent that edits a file
leaves a diff you can read before it goes anywhere; an agent that runs a
command does not.

## Selecting a mode

```sh
$ zcode run --mode planning "how should we split this module?"
$ zcode run --mode editing  "add the doc comments; don't run anything"
$ zcode run --mode auto     "split the module as discussed and make the tests pass"
$ zcode repl --mode planning
```

Or set a default in the config:

```json
{ "mode": "planning" }
```

Or per-invocation via the environment: `ZCODE_MODE=editing zcode run "..."`.
Precedence is flag → environment → file.

`build` is still accepted everywhere as a spelling of `auto` — it was the v0.1
name, and existing configs and scripts keep working.

## In the TUI

The status bar always names the current mode, colour-coded:

```
 ● ready  │  mode auto  │  openrouter/anthropic/claude-haiku-4.5  │  0 in / 0 out  │  $0.00
```

Change it mid-session without losing context:

```
/mode              # list the three, marking the active one
/mode planning     # switch
```

`Shift-Tab` cycles planning → editing → auto. The tool set changes immediately;
the conversation is untouched.

## What a restricted mode actually does

The guard has two layers.

**Layer one: the tool is never advertised.** In planning mode the model is not
told that `apply_patch` exists, so it usually does not try. Asked to edit a file
in planning mode, a real run produces a proposal and says why it stopped:

```sh
$ zcode run --mode planning "Rewrite main.go to remove the farewell function. Use apply_patch to do it."
```
```
**Final code will be:**
```go
package main
...
```

Would you like me to proceed with this plan? You'll need to use a write tool
(like `apply_patch`, `str_replace_editor`, or your editor directly) to make
these changes, as I can only propose them.
```

The file is byte-identical afterwards.

Editing mode behaves the same way about the shell:

```sh
$ zcode run --mode editing "Run the shell command 'go vet ./...' and report the output."
```
```
I appreciate the request, but I'm unable to run shell commands. The `shell`
tool is disabled in my environment, so I cannot execute `go vet ./...`.

However, I can help you in other ways: [...] you can run `go vet ./...`
yourself and share the output with me, and I'll help you fix any issues.
```

**Layer two: the engine refuses before the tool runs.** If a model calls a
withheld tool anyway, dispatch stops it:

```
! tool `apply_patch` denied: planning mode is read-only
zcode: tool error: tool `apply_patch` denied: planning mode is read-only
$ echo $?
1
```

The refusal is recorded as a `tool_denied` telemetry event, and the partial
session is still checkpointed.

Name spelling cannot get around it: aliases such as `lsp::rename_symbol` and
`zcode:skill` are canonicalised before the check, and the spec filter and the
dispatch gate call the *same* predicate (`domain::modes::denies`), so the list
the model sees can never disagree with the list it is allowed to use.

## A three-phase workflow

Plan, then edit, then let it verify:

```sh
$ zcode run --mode planning "how would you add retry logic to the HTTP client?"
# ... read the proposal, note the session id from the summary line ...

$ zcode run --session <id> --mode editing "write it, don't run anything yet"
# ... review the diff yourself ...

$ zcode run --session <id> --mode auto "now run the tests and fix what breaks"
```

Each call resumes the same session, so the plan is already in context. The mode
is recorded per session and on every telemetry event, so you can tell
afterwards which runs were allowed to write and which were allowed to execute.

---

Next: [MCP & LSP](09-mcp-and-lsp.md)
