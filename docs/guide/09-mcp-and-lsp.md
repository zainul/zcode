# 9. MCP & LSP

← [Agent modes](08-agent-modes.md) · [Index](README.md) · Next: [Multimodal input](10-multimodal.md)

Two ways to extend what the agent can reach: **MCP** for external data and
services, **LSP** for semantic understanding of code. Both are configured
declaratively **in the same config file as everything else** — there is no
separate MCP or LSP config — and both appear as ordinary tools.

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

### It is on by default

You do not have to configure anything. zcode works out what kind of project it
is in, and starts the matching language server if that server is installed:

| Project marker | Language | Server started |
|----------------|----------|----------------|
| `go.mod` | Go | `gopls serve` |
| `Cargo.toml` | Rust | `rust-analyzer` |
| `tsconfig.json`, `next.config.*`, `deno.json` | TypeScript / Next.js | `typescript-language-server --stdio` |
| `package.json` | JavaScript | `typescript-language-server --stdio` |

Next.js is not a separate entry: a Next.js project is a TypeScript project, and
`typescript-language-server` is the server for it. The aliases `nextjs`, `next`,
`node`, `nodejs`, `ts`, `tsx` all resolve to `typescript`; `golang` resolves to
`go`.

`zcode config` shows exactly what resolved:

```sh
$ zcode config
...
  lsp servers            1  (project looks like go)
    ▸ go           /Users/you/go/bin/gopls  [default]
```

Two rules keep this from being annoying:

- **A default is only started if its binary is on `PATH`.** No warning per
  session for the majority of people who do not write Go.
- **Only a server for *this* project's language is started.** A Go repo on a
  machine that also has `rust-analyzer` installed does not get `rust-analyzer`
  — it could answer nothing about Go, and would cost a process. If no marker
  identifies the directory at all, no default server is started.

Install the server for your stack and it just works:

```sh
go install golang.org/x/tools/gopls@latest          # Go
rustup component add rust-analyzer                  # Rust
npm i -g typescript-language-server typescript      # TS / JS / Next.js
```

### Overriding the defaults

Naming a server for a language replaces the default for that language. An
explicitly configured server is always started, even if its language is not the
one detected:

```json
{
  "lsp": {
    "servers": [
      { "language": "python", "command": "pyright-langserver", "args": ["--stdio"] },
      { "language": "rust",   "command": "rust-analyzer", "args": [], "env": [["RA_LOG", "error"]] }
    ]
  }
}
```

Turn the built-ins off entirely with `"defaults": false`:

```json
{ "lsp": { "defaults": false } }
```

In TOML:

```toml
[lsp]
defaults = false

[[lsp.servers]]
language = "python"
command = "pyright-langserver"
args = ["--stdio"]
```

### The tools

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

One server runs per session — the project's own. Per-extension routing across
several servers at once is not implemented yet. A language server that fails to
start is reported and skipped, exactly like MCP; in the TUI that warning appears
as a note row in the timeline rather than over the interface.

## Choosing between them

| Question | Use |
|----------|-----|
| "What does this symbol refer to?" | LSP |
| "What rows are in that table?" | MCP |
| "Where is this function used?" | LSP |
| "What does ticket ABC-123 say?" | MCP |

---

Next: [Multimodal input](10-multimodal.md)
