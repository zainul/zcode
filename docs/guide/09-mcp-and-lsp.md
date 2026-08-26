# 9. MCP & LSP

← [Agent modes](08-agent-modes.md) · [Index](README.md) · Next: [Multimodal input](10-multimodal.md)

Two ways to extend what the agent can reach: **MCP** for external data and
services, **LSP** for semantic understanding of code. Both are configured
declaratively and appear as ordinary tools.

Both are compiled in by default. `cargo build --release --no-default-features`
drops them if you want the smallest binary.

## MCP — external data sources

[Model Context Protocol](https://modelcontextprotocol.io) servers expose tools
over stdio JSON-RPC. Declare them and their tools join the namespace at startup.

```json
{
  "mcp": {
    "servers": [
      {
        "name": "everything",
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-everything"]
      },
      {
        "name": "postgres",
        "command": "mcp-server-postgres",
        "args": ["--dsn", "postgres://localhost/app"],
        "env": [["PGPASSWORD", "secret"]]
      }
    ]
  }
}
```

In TOML:

```toml
[[mcp.servers]]
name = "everything"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]
```

Verify discovery:

```sh
$ zcode tools list
read                         Read a UTF-8 text file ...
...
mcp__everything__echo        Echoes back the input
mcp__postgres__query         Run a read-only SQL query
```

Tools are namespaced `mcp__<server>__<tool>` so two servers can expose the same
tool name without colliding.

**A broken server never breaks the agent.** If a server fails to spawn or does
not answer, it is reported and skipped, and the run proceeds with whatever else
came up:

```sh
$ zcode tools list
warning: mcp server `postgres` failed to start: mcp spawn failed: mcp-server-postgres: No such file or directory (os error 2)
read                         Read a UTF-8 text file ...
```

Every request has a deadline (`timeout_ms`), so a wedged server cannot hang the
agent — it produces an error the model can react to. Server processes are
killed when `zcode` exits.

## LSP — semantic code intelligence

Language servers let the agent ask what a symbol *means* rather than grepping
for it.

```json
{
  "lsp": {
    "servers": [
      { "language": "rust", "command": "rust-analyzer", "args": [] }
    ]
  }
}
```

That adds four tools:

| Tool | Arguments | Answers |
|------|-----------|---------|
| `lsp__goto_definition` | `path`, `line`, `character` | Where is this defined? |
| `lsp__find_references` | `path`, `line`, `character` | Who calls this? |
| `lsp__hover` | `path`, `line`, `character` | What is its type and doc? |
| `lsp__rename_symbol` | + `new_name` | Which edits would rename it? |

Positions are 0-based going in (the LSP convention) and reported back 1-based,
matching what an editor shows.

```sh
$ zcode run "who calls the greet function?"
· lsp__find_references
  lsp__find_references: file:///home/you/zcode-demo/src/main.rs:2:20
```

Two design points:

- **Renames are advice.** `lsp__rename_symbol` returns the edits the server
  proposes; the agent then applies them through the file tools. There is
  exactly one code path that writes to disk.
- **The server sees your edits.** After a `write` or `str_replace_editor`, the
  new text is pushed to the language server, so subsequent lookups reflect the
  current state rather than the version on disk at startup.

Currently the first server that starts successfully is used for the whole run;
per-extension routing across several servers is not implemented yet. A missing
language server is reported and skipped, exactly like MCP.

## Choosing between them

| Question | Use |
|----------|-----|
| "What does this symbol refer to?" | LSP |
| "What rows are in that table?" | MCP |
| "Where is this function used?" | LSP |
| "What does ticket ABC-123 say?" | MCP |

---

Next: [Multimodal input](10-multimodal.md)
