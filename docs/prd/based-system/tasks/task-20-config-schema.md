# Task 20 — Config Schema Extension (provider, MCP, LSP, allowlist, skills, modes)

**Related PRD sections:** §3.7 Configuration & Allowed Commands (FR-CONFIG-01..06), §3.5 Agent Mode Switching (FR-MODE-03), §3.6 Structured Output, §5 Out of Scope, §8 Assumptions (env > file precedence)
**Depends on:** task-01, task-07 (extend existing `infra/config`)
**Status:** Done
**Priority:** High (foundation — `wire()` and every other task consume `Config`)

## Objective

Extend the v0.1 `Config` model and `Loader` so `zcode.toml` + `ZCODE_*` env vars drive provider selection, MCP server list, LSP server registry, shell allowlist, skills dir, agent mode, and loop caps. Secrets are resolved **by name** from env at compose time; nothing is persisted to disk. Env always overrides file (FR-CONFIG-04 / §8).

## Step-by-step

### 1. Extend `crates/infra/config/src/lib.rs`

Add nested sections to `Config`:

```rust
pub struct Config {
    pub provider: Provider,            // FR-CONFIG-02
    pub model: String,                // already exists, kept
    pub api_key_env: String,          // name of env var, e.g. "ZCODE_OPENAI_API_KEY" (FR-CONFIG-03)
    pub base_url: Option<String>,     // override default endpoint (vLLM/Ollama/OpenAI-compatible)
    pub working_dir: PathBuf,         // already exists
    pub env: Vec<(String, String)>,   // already exists
    pub timeout_ms: u64,              // already exists
    pub max_turns: u64,               // FR-LOOP-02 (default 20)
    pub max_tokens: u64,              // FR-LOOP-03 (default 16384)
    pub max_tool_output_chars: usize,  // FR-LOOP-04 (default 16000)
    pub mcp_servers: Box<[McpServerConfig]>, // FR-MCP-02
    pub lsp_servers: Box<[LspServerConfig]>, // FR-LSP-03
    pub shell_allowed: Box<[String]>,         // FR-CONFIG-04 (regex patterns)
    pub skills_dir: PathBuf,                 // FR-CONFIG-06 (default .zcode/skills)
    pub mode: AgentMode,                     // FR-MODE-01 (Planning/Build; default Build)
}
```

Add supporting types (pure structs/enums, no deps):

```rust
pub enum Provider { Openai, Anthropic, Openrouter, Ollama, Vllm, OpenaiCompatible }
// deserialize from "openai" | "anthropic" | "openrouter" | "ollama" | "vllm" | "openai-compatible"
pub struct McpServerConfig { pub name: String, pub command: String, pub args: Box<[String]>, pub env: Box<[(String,String)]> }
pub struct LspServerConfig { pub language: String, pub command: String, pub args: Box<[String]>, pub env: Box<[(String,String)]> }
```

`AgentMode` is re-exported from `domain` (it is a domain concept per §4.4 and task-16 planning-build gating); `infra/config` deserializes it.

### 2. `Loader::load()` extension

Keep the existing env-over-file merge. Add:
- Deserialize `[mcp.servers]` (TOML array of tables) and `[lsp]` / `[lsp.servers]` tables.
- Parse `shell.allowed` as a `Vec<String>` of regex strings.
- Parse `provider` to `Provider` enum (unknown → `ConfigError::UnknownProvider`).
- `resolve_api_key(&self) -> Result<String, ConfigError>`: `std::env::var(&self.api_key_env)` — never log the value.
- Default `shell_allowed` = `["echo .*", "ls .*", "cd .*", "cat .*"]`; empty list = deny all (fail-safe).

```rust
impl Loader {
    pub fn load(&self) -> Result<Config, ConfigError> { /* env overlays file */ }
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        std::env::var(&self.api_key_env).map_err(|_| ConfigError::MissingSecret(self.api_key_env.clone()))
    }
}
```

Add `ConfigError` variants: `UnknownProvider(String)`, `MissingSecret(String)`, `InvalidRegex(String)` (wrapped from `regex::Error` — note: `regex` is a dev/build dep only for validation; do **not** pull `regex` into the runtime `Config` path — store patterns as `String` and compile in `crates/tools` where the allowlist is enforced, keeping infra/config dep-light).

### 3. Defaults tuned to PRD

`max_turns = 20`, `max_tokens = 16384`, `max_tool_output_chars = 16000`, `mode = Build`, `skills_dir = <working_dir>/.zcode/skills`, `provider = Openai`, `model = "gpt-4o-mini"` (unchanged default).

### 4. Extend `examples/zcode.example.toml`

Document every new key with comments + the secrets-by-name rule (FR-CONFIG-03).

### 5. Tests (`#[cfg(test)]`)

- `env_overrides_file_and_provider`: set `ZCODE_PROVIDER=anthropic`, assert parsed `Provider::Anthropic`.
- `empty_allowed_is_deny_all`: config with `shell_allowed = []` → `config.shell_allowed.is_empty()` true.
- `unknown_provider_errors`: file has `provider = "bogus"` → `ConfigError::UnknownProvider`.
- `resolve_api_key_missing`: `api_key_env = "ZCODE_NONEXISTENT"` → `MissingSecret`.
- `mcp_servers_parsed`: two `[[mcp.servers]]` tables deserialize to 2 entries.
- `default_caps_match_prd`: `Config::default().max_turns == 20`.

**Do NOT import `regex` into `infra/config`'s runtime deps** — validation of the patterns happens in task-15 (`crates/tools`, which already pulls `regex`). This keeps `make check-deps` edge count low for config (L3).

## Test-case scenario

- `zcode.toml` sets `provider="openrouter"`, `model="google/gemini-2.0-flash"`, `api_key_env="ZCODE_OPENROUTER_API_KEY"`, two MCP servers, three LSP servers, `shell.allowed=["git .*", "cargo .*"]`, `mode="planning"`, `skills_dir=".zcode/skills"`. Loader merges, `ZCODE_MODEL=gpt-4o` overrides only the model, `resolve_api_key()` reads `ZCODE_OPENROUTER_API_KEY` from env.

## How to verify

```
cargo test -p infra-config
cargo clippy -p infra-config -- -D warnings
cargo tree -p infra-config     # must show only: domain, serde, toml  (no reqwest/regex)
cargo doc -p infra-config --no-deps
```

**Pass criteria:** round-trip of all new keys via `tempfile` + `toml` string; env overrides file; unknown provider is a typed error (not a panic — NFR-REL-01); `cargo tree -p infra-config` has no `regex`/`reqwest` edges; `resolve_api_key` never prints the secret.

## Success metric mapping

- M1.2, M1.3 (tests + lint), M1.4 (acyclic: config → domain only), NFR-SEC-01 (secrets-by-name, never persisted), NFR-REL-02 (typed errors), L3 edge count (config direct deps ≤ 6), FR-CONFIG-01..06, FR-MODE-03 (mode in config).

## Open questions resolved here

- **Q (DQ11):** Single `Config` struct vs. split files? → single struct, nested sections (chosen).
- **Secrets storage:** env-by-name at `wire()`, confirmed here; `zcode.toml.local` stays gitignored (already in `.gitignore`).
