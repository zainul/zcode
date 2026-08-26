# 8. Agent modes

← [Tools & safety](07-tools-and-safety.md) · [Index](README.md) · Next: [MCP & LSP](09-mcp-and-lsp.md)

Two modes swap both the system prompt and the available tool set.

| | `build` (default) | `planning` |
|---|---|---|
| System prompt | act autonomously, edit, then verify | propose changes and ask for confirmation |
| Editing tools | available | **withheld** |
| Read-only tools | available | available |
| Typical use | "make this change" | "what would you change?" |

## Selecting a mode

```sh
$ zcode run --mode planning "how should we split this module?"
$ zcode run --mode build "split the module as discussed"
$ zcode repl --mode planning
```

Or set a default in the config:

```json
{ "mode": "planning" }
```

Or per-invocation via the environment: `ZCODE_MODE=planning zcode run "..."`.
Precedence is flag → environment → file.

## What planning mode withholds

Withheld: `write`, `str_replace_editor`, `apply_patch`, `shell`, `zcode_skill`,
`lsp__rename_symbol`.

Still available: `read`, `list_dir`, `lsp__hover`, `lsp__goto_definition`,
`lsp__find_references`, and MCP tools.

The guard has two layers. Editing tools are not advertised to the model at all,
so it usually will not try; and if it tries anyway, the engine refuses before
the tool runs:

```sh
$ zcode run --mode planning "add a farewell function"
I'll add the function.
· apply_patch
! tool `apply_patch` denied in planning mode
zcode: tool error: tool `apply_patch` denied in planning mode
$ echo $?
1
```

The file is untouched. The refusal is recorded in the telemetry stream as a
`tool_denied` event, and the partial session is still checkpointed.

Name spelling cannot get around it: aliases such as `lsp::rename_symbol` and
`zcode:skill` are canonicalised before the check, and both the gate and the
dispatcher use the same function, so they cannot disagree.

## A two-phase workflow

Plan first, review, then execute:

```sh
$ zcode run --mode planning "how would you add retry logic to the HTTP client?"
# ... read the proposal ...

$ zcode run --session <id-from-the-summary-line> --mode build "do it"
```

Because the second call resumes the same session, the plan is already in
context and the model does not have to rediscover it. The mode is recorded per
session and on every telemetry event, so you can tell afterwards which runs
were allowed to write.

---

Next: [MCP & LSP](09-mcp-and-lsp.md)
