# Task 15 — `crates/tools`: Native Tools + Merging ToolRegistry

**Related PRD sections:** §3.3 Extensible Tool System (FR-TOOL-FS-01/02/03, FR-TOOL-SHELL-01/02; FR-MCP-03/04/05; FR-LSP-02), §3.7 Configuration (FR-CONFIG-04/05/06), §8 DQ1 (PTY defer), DQ10 (Tool trait in domain, registry in crates/tools)
**Depends on:** task-02 (Domain — `Tool` trait + `ToolRegistryPort` + `ToolSpec`/`ToolResult` defined in §4.2), task-05 (StdFs), task-06 (StdShell), task-13 (McpClient), task-14 (LspClient, optional)
**Status:** Done
**Priority:** High (the engine loop dispatches through this registry)

## Objective

Provide the **single namespace of tools** the engine sees. `ToolRegistry` merges (a) native tools implemented in-process via `domain::Tool`, (b) MCP-backed tools exposed through an `McpPort`, and (c) LSP-backed semantic tools exposed through an `LspPort`. The `shell` native tool enforces the **command allowlist** via a `GuardedShell` decorator (FR-CONFIG-05). Native file edits are **in-process** (no shell `sed`/`grep`) per FR-TOOL-FS-03.

## Step-by-step

### 1. New crate `crates/tools`

`Cargo.toml`:
```toml
[dependencies]
domain = { path = "../../domain", version = "0.1.0" }
infra-filesystem = { path = "../infra/filesystem", version = "0.1.0" }
infra-shell = { path = "../infra/shell", version = "0.1.0" }
infra-mcp = { path = "../infra/mcp", version = "0.1.0", optional = true }
infra-lsp = { path = "../infra/lsp", version = "0.1.0", optional = true }
infra-config = { path = "../infra/config", version = "0.1.0" }
regex = "1.10"        # ONLY here, for shell-allowed pattern matching
thiserror = { workspace = true }

[features]
default = []
mcp = ["dep:infra-mcp"]
lsp = ["dep:infra-lsp"]
```
`regex` is allowed **only** in this crate (not in domain/config) so `make check-deps` keeps infra/config dep-light (L3).

### 2. `GuardedShell` decorator (FR-CONFIG-05 / NFR-SEC-02)

```rust
pub struct GuardedShell { inner: StdShell, allowed: Box<[Regex]> }
impl GuardedShell {
    pub fn new(inner: StdShell, allowed: &[String]) -> Result<Self, ShellToolError>; // compiles regexes; empty list = deny all
    pub fn run_guarded(&mut self, cmd: &ShellCommand) -> Result<String, Box<dyn Error>> {
        // FR-CONFIG-04: EVERY space-token/segment must match >=1 allowed regex.
        if !is_allowed(&cmd.command, &self.allowed) { return Err(ShellBlocked(cmd.command.clone())) }
        self.inner.run(cmd)
    }
}
fn is_allowed(cmd: &str, allowed: &[Regex]) -> bool {
    // split on whitespace; each segment must match >=1 regex (regex uses .is_match on the segment)
}
```
Empty `allowed` → no segment matches → deny all (default-fail, M2.5). Default set from config: `["echo .*","ls .*","cd .*","cat .*"]`.

### 3. Native `Tool` impls

- `FsReadTool` — `read(path)` via `StdFs`; result = file contents (or error string).
- `FsWriteTool` — `write(path, content)` via `StdFs`; **atomic** (write-temp + rename) (FR-TOOL-FS-02).
- `StrReplaceTool` — `str_replace_editor` family: `view`/`create`/`str_replace`/`list_dir` (FR-TOOL-FS-03). `str_replace(path, old_str, new_str)` does an in-process `find+replace` (first occurrence wins; error if `old_str` absent). **No `sed`/`awk`.**
- `ListDirTool` — `list_dir(path)` (FR-TOOL-FS-03).
- `ShellTool` — `shell(command, cwd?, timeout?)` delegating to `GuardedShell::run_guarded` (FR-TOOL-SHELL-01). Returns `stdout|stderr|exit:N`.
- `SkillTool` — `zcode:skill <name>` reads `<skills_dir>/<name>.md` (FR-OUTPUT-09). **Path-traversal guarded**: resolved path must be inside `skills_dir` (uses `canonicalize` + prefix check).

Each implements `domain::Tool` with a `ToolSpec` (name, description, params JSON schema) so the LLM sees the signatures.

### 4. `ToolRegistry` (impl `ToolRegistryPort`)

```rust
pub struct ToolRegistry {
    native: Vec<(String, Box<dyn Tool>)>,          // name -> tool
    mcp: Vec<(String, Box<dyn McpPort>)>,          // server name -> client, tools under "mcp::<server>::<name>"
    lsp: Option<(String, Box<dyn LspPort>)>,      // optional; tools under "lsp::goto_definition" etc.
    allowed: ...,  // shell allowlist passed to ShellTool
}
impl ToolRegistryPort for ToolRegistry {
    fn list(&self) -> Box<[ToolSpec]> { /* native + mcp.list_tools (prefixed) + lsp specs */ }
    fn call(&mut self, name: &str, args_json: &str) -> Result<ToolResult, Box<dyn Error>> {
        // dispatch: native → self.tool.call; "mcp::<srv>::<n>" → McpPort::call; "lsp::<op>" → LspPort method
    }
    fn is_native(&self, name: &str) -> bool { /* native-only, for planning-mode gating (FR-MODE-01) */ }
}
```

Namespace convention: `mcp::<server_name>::<tool_name>` for MCP tools; `lsp::goto_definition`, `lsp::find_references`, `lsp::hover`, `lsp::rename_symbol` for LSP tools. Native tools have bare names (`read`, `write`, `str_replace_editor`, `list_dir`, `shell`, `zcode:skill`).

### 5. `spawn` (PTY) remains deferred

`ShellPort::spawn` continues to return `Pty(PtyError)` (v0.1 behavior). The persistent PTY shell (FR-TOOL-SHELL-02) lands in the `pty` feature of v0.2.1. The single-run `shell` tool via `run()` is fully implemented here.

### 6. Tests

- `shell_allowed_runs_echo`: `GuardedShell` with default allowlist, `run("echo hi")` → succeeds, output contains `hi`.
- `shell_blocked_rm_rf`: allowlist present, `run("rm -rf /")` → `ShellBlocked` error (M1.10).
- `shell_deny_all_empty`: `allowed = []` → any command refused (M2.5).
- `shell_partial_token_denied`: `run("echo foo; rm -rf /")` → denied because `rm` segment matches no regex.
- `str_replace_edit_roundtrip`: tempdir file, `str_replace` replaces old→new, file content updated.
- `write_atomic_on_subdir`: `write` creates parent dirs then renames temp file (FR-TOOL-FS-02).
- `skill_path_traversal_blocked`: `zcode:skill "../secret"` → error (path outside skills_dir).
- `registry_merges_native_and_mcp_specs`: build a `ToolRegistry` with 1 native tool + 1 fake `McpPort`; `list()` returns both; `call("read", …)` dispatches to the native tool, `call("mcp::srv::fn", …)` to the fake MCP.
- `is_native_planning_filter`: `is_native("write")==false`? **NO** — `write` IS native; planning mode excludes *execute-side native tools* (`write`, `str_replace`, `shell`, `rename_symbol`) via the **mode policy** in task-16, not `is_native`. `is_native("read")==true`, `is_native("mcp::srv::fn")==false`. (Clarifies FR-MODE-01 boundary.)

Fakes: provide `FakeMcp` (returns canned `McpToolDef`s) and `FakeLsp` (returns canned `LspLocation`) as `#[cfg(test)]` structs so registry dispatch is hermetic.

## Test-case scenario

- `zcode tools list` shows `read`, `write`, `str_replace_editor`, `list_dir`, `shell`, `zcode:skill`, plus any discovered `mcp::<srv>::*` and `lsp::*` tools.
- `zcode run "shell: echo hi"` → `shell` tool runs (allowlisted); `zcode run "shell: rm -rf /tmp/x"` → refused with a clear error that is fed back to the LLM.

## How to verify

```
cargo test -p tools
cargo clippy -p tools -- -D warnings
cargo tree -p tools            # deps: domain, infra-filesystem, infra-shell, infra-config, regex (+ optional mcp/lsp)
```

**Pass criteria:** native tools perform real FS/shell edits in-process; shell allowlist enforces segment matching (FR-CONFIG-04); empty allowlist denies all (M2.5); MCP/LSP tools are namespaced and dispatch correctly; path traversal blocked for skills; zero `unsafe` in this crate; `cargo tree -p tools` shows the expected edge set.

## Success metric mapping

- M1.3 (clippy), M1.10 (shell allowlist blocks `rm -rf /`), M2.5 (deny-all on empty), FR-TOOL-FS-01/02/03, FR-TOOL-SHELL-01, FR-MCP-03/04/05 (registry merges MCP), FR-LSP-02 (registry exposes LSP), FR-CONFIG-04/05/06, FR-OUTPUT-09 (skill tool), NFR-SEC-02 (decorator + default-deny), DQ10 (Tool trait in domain, registry concrete here).

## Notes / risks

- The allowlist checks **every whitespace segment** against **every** regex (regex semantics: `.is_match` is substring match; for safety the patterns are anchored with explicit `.*`/prefixes in the default set). A command like `echo hi && rm -rf /` has segments `["echo", "hi", "&&", "rm", "-rf", "/"]` — `rm` will fail to match any default regex → denied. (Defense in depth; the engine also rate-limits tool rounds.)
