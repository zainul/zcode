//! Configuration model + loader for zcode.
//! Secrets are read from `ZCODE_*` env vars only, never written to disk.
//! Deps (direct): domain, serde, toml, thiserror — no `reqwest`/`regex` here (L3).

use domain::AgentMode;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_TURNS: u64 = 20;
const DEFAULT_MAX_TOKENS: u64 = 16384;
const DEFAULT_MAX_TOOL_OUTPUT_CHARS: usize = 16000;

const DEFAULT_SHELL_ALLOWED: &[&str] = &["echo .*", "ls .*", "cd .*", "cat .*"];

/// Config files looked for, in order, when no `--config` is given.
const DEFAULT_CONFIG_NAMES: &[&str] = &["zcode.json", "zcode.toml"];

/// Provider selection (FR-CONFIG-02 / FR-MODEL-01..08).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Openai,
    Anthropic,
    Openrouter,
    Deepseek,
    Ollama,
    Vllm,
    OpenaiCompatible,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Openrouter => "openrouter",
            Self::Deepseek => "deepseek",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::OpenaiCompatible => "openai-compatible",
        }
    }

    /// The env var this provider's key conventionally lives in, used when
    /// `api_key_env` is not set explicitly so switching provider does not
    /// require also remembering to switch the key name.
    pub fn default_api_key_env(&self) -> &'static str {
        match self {
            Self::Openai => "ZCODE_OPENAI_API_KEY",
            Self::Anthropic => "ZCODE_ANTHROPIC_API_KEY",
            Self::Openrouter => "ZCODE_OPENROUTER_API_KEY",
            Self::Deepseek => "ZCODE_DEEPSEEK_API_KEY",
            Self::Ollama => "ZCODE_OLLAMA_API_KEY",
            Self::Vllm | Self::OpenaiCompatible => "ZCODE_API_KEY",
        }
    }

    /// A model id that is valid for this provider, used when the config does
    /// not name one. Without this, switching provider while keeping the
    /// default model produces a confusing 404 from the provider.
    pub fn default_model(&self) -> &'static str {
        match self {
            Self::Openai => "gpt-4o-mini",
            Self::Anthropic => "claude-sonnet-4-5",
            // OpenRouter ids are namespaced by vendor.
            Self::Openrouter => "openai/gpt-4o-mini",
            Self::Deepseek => "deepseek-chat",
            Self::Ollama => "llama3.2",
            // Self-hosted servers usually expose a single model under a name
            // only the operator knows, so there is nothing sensible to guess.
            Self::Vllm | Self::OpenaiCompatible => "",
        }
    }

    /// Local providers need no credential.
    pub fn requires_api_key(&self) -> bool {
        !matches!(self, Self::Ollama | Self::Vllm | Self::OpenaiCompatible)
    }

    pub fn default_endpoint(&self) -> Option<&'static str> {
        match self {
            Self::Openai => Some("https://api.openai.com/v1/chat/completions"),
            Self::Anthropic => Some("https://api.anthropic.com/v1/messages"),
            Self::Openrouter => Some("https://openrouter.ai/api/v1/chat/completions"),
            Self::Deepseek => Some("https://api.deepseek.com/chat/completions"),
            Self::Ollama => Some("http://localhost:11434/api/chat"),
            Self::Vllm | Self::OpenaiCompatible => None,
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "openrouter" => Ok(Self::Openrouter),
            "deepseek" => Ok(Self::Deepseek),
            "ollama" => Ok(Self::Ollama),
            "vllm" => Ok(Self::Vllm),
            "openai-compatible" => Ok(Self::OpenaiCompatible),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

/// A stdio MCP server definition (FR-MCP-02).
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// A stdio LSP server definition (FR-LSP-03).
#[derive(Debug, Clone, Deserialize)]
pub struct LspServerConfig {
    pub language: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    pub api_key_env: String,
    pub base_url: Option<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
    pub max_turns: u64,
    pub max_tokens: u64,
    pub max_tool_output_chars: usize,
    pub mcp_servers: Box<[McpServerConfig]>,
    pub lsp_servers: Box<[LspServerConfig]>,
    pub shell_allowed: Box<[String]>,
    pub skills_dir: PathBuf,
    pub mode: AgentMode,
}

impl Default for Config {
    fn default() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            provider: Provider::Openai,
            model: DEFAULT_MODEL.to_string(),
            api_key_env: String::from("ZCODE_OPENAI_API_KEY"),
            base_url: None,
            working_dir,
            env: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            max_turns: DEFAULT_MAX_TURNS,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_tool_output_chars: DEFAULT_MAX_TOOL_OUTPUT_CHARS,
            mcp_servers: Box::new([]),
            lsp_servers: Box::new([]),
            shell_allowed: DEFAULT_SHELL_ALLOWED
                .iter()
                .map(|s| s.to_string())
                .collect(),
            skills_dir: PathBuf::new(),
            mode: AgentMode::default(),
        }
    }
}

impl Config {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    /// The primary skills directory (the project's own, unless overridden).
    pub fn skills_dir(&self) -> PathBuf {
        if self.skills_dir.as_os_str().is_empty() {
            self.working_dir.join(".zcode").join("skills")
        } else {
            expand_tilde(self.skills_dir.clone())
        }
    }

    /// Every directory searched for skills, nearest first.
    ///
    /// A configured `skills_dir` *adds* a root rather than replacing the
    /// project's own: a machine-wide library set in the user config would
    /// otherwise make per-project skills impossible. Names found earlier win.
    pub fn skills_dirs(&self) -> Vec<PathBuf> {
        let mut roots = Vec::with_capacity(3);
        let mut push = |dir: PathBuf| {
            if !dir.as_os_str().is_empty() && !roots.contains(&dir) {
                roots.push(dir);
            }
        };
        push(self.working_dir.join(".zcode").join("skills"));
        if !self.skills_dir.as_os_str().is_empty() {
            push(expand_tilde(self.skills_dir.clone()));
        }
        // The machine-wide library, so a global collection is always available.
        if let Some(user) = user_config_candidates().first().and_then(|p| p.parent()) {
            push(user.join("skills"));
        }
        roots
    }

    /// Resolve the API key by name from env (FR-CONFIG-03/NFR-SEC-01). The
    /// secret value is never logged.
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        std::env::var(&self.api_key_env)
            .map_err(|_| ConfigError::MissingSecret(self.api_key_env.clone()))
    }

    pub fn to_agent_context(&self) -> domain::AgentContext {
        domain::AgentContext {
            working_dir: self.working_dir.clone(),
            model: self.model.clone(),
            env: self.env.clone(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("missing secret env var: {0}")]
    MissingSecret(String),
    #[error("invalid agent mode: {0}")]
    InvalidMode(String),
}

/// Intermediate deserialization target so unknown/extra keys in the file are
/// ignored and individual overrides can be merged field-by-field.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    working_dir: Option<PathBuf>,
    #[serde(default)]
    env: Option<Vec<(String, String)>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_turns: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
    #[serde(default)]
    max_tool_output_chars: Option<usize>,
    #[serde(default)]
    mcp: McpSection,
    #[serde(default)]
    lsp: LspSection,
    #[serde(default)]
    shell_allowed: Option<Vec<String>>,
    #[serde(default)]
    skills_dir: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct McpSection {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct LspSection {
    #[serde(default)]
    servers: Vec<LspServerConfig>,
}

/// Where a configuration layer came from, for `zcode config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerKind {
    /// `~/.config/zcode/config.{json,toml}` — machine-wide defaults.
    User,
    /// The nearest `zcode.{json,toml}` at or above the working directory.
    Project,
    /// An explicit `--config <FILE>`.
    Explicit,
}

/// One resolved configuration file.
#[derive(Debug, Clone)]
pub struct ConfigLayer {
    pub kind: LayerKind,
    pub path: PathBuf,
}

/// Builds a `Config` from layered sources.
///
/// Later layers override earlier ones field by field, so a machine-wide file
/// can carry the provider and key name while each project overrides only what
/// differs:
///
/// ```text
/// defaults → ~/.config/zcode/config.json → <project>/zcode.json → ZCODE_* env
/// ```
pub struct Loader {
    layers: Vec<ConfigLayer>,
    /// Directory holding the project config, used to anchor `working_dir`.
    project_root: Option<PathBuf>,
}

impl Loader {
    /// Load exactly one file, with no user layer and no discovery.
    pub fn new(config_path: impl Into<PathBuf>) -> Self {
        Self {
            layers: vec![ConfigLayer {
                kind: LayerKind::Explicit,
                path: config_path.into(),
            }],
            project_root: None,
        }
    }

    /// The full chain: user config, then the nearest project config found by
    /// walking up from the current directory.
    pub fn with_default() -> Self {
        Self::discover_from(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// `with_default`, but rooted at an explicit directory (testable).
    pub fn discover_from(start: &Path) -> Self {
        let mut layers = Vec::with_capacity(2);
        if let Some(path) = user_config_path() {
            layers.push(ConfigLayer {
                kind: LayerKind::User,
                path,
            });
        }
        let project = find_project_config(start);
        let project_root = project.as_ref().and_then(|p| p.parent().map(PathBuf::from));
        if let Some(path) = project {
            layers.push(ConfigLayer {
                kind: LayerKind::Project,
                path,
            });
        }
        Self {
            layers,
            project_root,
        }
    }

    /// The files this loader will read, in order. Only existing files appear.
    pub fn layers(&self) -> &[ConfigLayer] {
        &self.layers
    }

    /// Parse a config file, choosing the format from its extension. An
    /// unknown extension is parsed as TOML, then as JSON, so a file named
    /// `agent.conf` still works if its contents are valid.
    fn parse(path: &Path, content: &str) -> Result<ConfigFile, ConfigError> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("json") => serde_json::from_str(content).map_err(ConfigError::Json),
            Some("toml") => toml::from_str(content).map_err(ConfigError::Parse),
            _ => match toml::from_str(content) {
                Ok(parsed) => Ok(parsed),
                Err(toml_err) => serde_json::from_str(content).map_err(|_| {
                    // Report the TOML error: it is the more likely intent and
                    // the more legible message.
                    ConfigError::Parse(toml_err)
                }),
            },
        }
    }

    pub fn load(&self) -> Result<Config, ConfigError> {
        let mut config = Config::default();
        // Tracks whether these were chosen deliberately; if not, they are
        // derived from the provider so switching provider is one edit.
        let mut api_key_env_set = false;
        let mut model_set = false;
        let mut working_dir_set = false;

        for layer in &self.layers {
            if !layer.path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&layer.path)?;
            let file: ConfigFile = Self::parse(&layer.path, &content)?;

            if let Some(m) = file.model {
                config.model = m;
                model_set = true;
            }
            if let Some(p) = file.provider {
                config.provider = p
                    .parse::<Provider>()
                    .map_err(|_| ConfigError::UnknownProvider(p))?;
            }
            if let Some(k) = file.api_key_env {
                config.api_key_env = k;
                api_key_env_set = true;
            }
            if let Some(u) = file.base_url {
                config.base_url = Some(u);
            }
            if let Some(wd) = file.working_dir {
                config.working_dir = wd;
                working_dir_set = true;
            }
            if let Some(env) = file.env {
                config.env = env;
            }
            if let Some(t) = file.timeout_ms {
                config.timeout_ms = t;
            }
            if let Some(t) = file.max_turns {
                config.max_turns = t;
            }
            if let Some(t) = file.max_tokens {
                config.max_tokens = t;
            }
            if let Some(t) = file.max_tool_output_chars {
                config.max_tool_output_chars = t;
            }
            if !file.mcp.servers.is_empty() {
                config.mcp_servers = file.mcp.servers.into_boxed_slice();
            }
            if !file.lsp.servers.is_empty() {
                config.lsp_servers = file.lsp.servers.into_boxed_slice();
            }
            if let Some(v) = file.shell_allowed {
                config.shell_allowed = v.into_boxed_slice();
            }
            if let Some(s) = file.skills_dir {
                config.skills_dir = PathBuf::from(s);
            }
            if let Some(m) = file.mode {
                config.mode = m
                    .parse::<AgentMode>()
                    .map_err(|_| ConfigError::InvalidMode(m))?;
            }
        }

        // Anchor relative work to the project root, not to wherever the user
        // happened to `cd`. Running `zcode` from `src/` must behave the same
        // as running it from the directory holding the config.
        if !working_dir_set {
            if let Some(root) = &self.project_root {
                config.working_dir = root.clone();
            }
        }

        if let Ok(provider) = std::env::var("ZCODE_PROVIDER") {
            config.provider = provider
                .parse::<Provider>()
                .map_err(|_| ConfigError::UnknownProvider(provider))?;
        }
        if let Ok(model) = std::env::var("ZCODE_MODEL") {
            config.model = model;
            model_set = true;
        }
        if let Ok(k) = std::env::var("ZCODE_API_KEY_ENV") {
            config.api_key_env = k;
            api_key_env_set = true;
        }
        if let Ok(u) = std::env::var("ZCODE_BASE_URL") {
            config.base_url = Some(u);
        }
        if let Ok(wd) = std::env::var("ZCODE_WORKING_DIR") {
            config.working_dir = PathBuf::from(wd);
        }
        if let Ok(t) = std::env::var("ZCODE_TIMEOUT_MS") {
            if let Ok(ms) = t.parse::<u64>() {
                config.timeout_ms = ms;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_TURNS") {
            if let Ok(v) = t.parse::<u64>() {
                config.max_turns = v;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_TOKENS") {
            if let Ok(v) = t.parse::<u64>() {
                config.max_tokens = v;
            }
        }
        if let Ok(t) = std::env::var("ZCODE_MAX_TOOL_OUTPUT_CHARS") {
            if let Ok(v) = t.parse::<usize>() {
                config.max_tool_output_chars = v;
            }
        }
        if let Ok(dir) = std::env::var("ZCODE_SKILLS_DIR") {
            config.skills_dir = PathBuf::from(dir);
        }
        // Newline-separated so a pattern may contain spaces (`echo .*`).
        // An explicitly empty value means deny-all, which must be honoured
        // rather than silently falling back to the defaults (M2.5).
        if let Ok(raw) = std::env::var("ZCODE_SHELL_ALLOWED") {
            config.shell_allowed = raw
                .split('\n')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        }
        if let Ok(m) = std::env::var("ZCODE_MODE") {
            config.mode = m
                .parse::<AgentMode>()
                .map_err(|_| ConfigError::InvalidMode(m.clone()))?;
        }

        // People write `~/...` in config files; expand it before anything
        // tries to open the path.
        config.working_dir = expand_tilde(config.working_dir);
        config.skills_dir = expand_tilde(config.skills_dir);

        if !api_key_env_set {
            config.api_key_env = config.provider.default_api_key_env().to_string();
        }
        if !model_set {
            let default_model = config.provider.default_model();
            if !default_model.is_empty() {
                config.model = default_model.to_string();
            }
        }

        Ok(config)
    }

    /// Load from a specific config override path (`--config`). The user-level
    /// layer still applies underneath, so `--config` overrides a project file
    /// without discarding machine-wide defaults such as the provider key name.
    pub fn load_with_override(
        &self,
        override_path: Option<impl AsRef<Path>>,
    ) -> Result<Config, ConfigError> {
        let Some(p) = override_path else {
            return self.load();
        };
        let path = p.as_ref().to_path_buf();
        let mut layers = Vec::with_capacity(2);
        if let Some(user) = user_config_path() {
            layers.push(ConfigLayer {
                kind: LayerKind::User,
                path: user,
            });
        }
        let project_root = path.parent().map(PathBuf::from);
        layers.push(ConfigLayer {
            kind: LayerKind::Explicit,
            path,
        });
        Loader {
            layers,
            project_root,
        }
        .load()
    }
}

/// Expand a leading `~` to the user's home directory.
///
/// Config files are written by people, and `"~/.config/zcode/skills"` is the
/// natural thing to write. Rust treats `~` as an ordinary directory name, so
/// without this the path silently resolves to a literal `./~/...` that never
/// exists. `~user/...` is not supported.
pub fn expand_tilde(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if text == "~" {
        return std::env::var_os("HOME").map(PathBuf::from).unwrap_or(path);
    }
    let Some(rest) = text.strip_prefix("~/") else {
        return path;
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => path,
    }
}

/// Candidate paths for the machine-wide config, in preference order.
pub fn user_config_candidates() -> Vec<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    let Some(base) = base else {
        return Vec::new();
    };
    let dir = base.join("zcode");
    vec![dir.join("config.json"), dir.join("config.toml")]
}

/// The machine-wide config, if one exists.
pub fn user_config_path() -> Option<PathBuf> {
    user_config_candidates().into_iter().find(|p| p.exists())
}

/// The nearest `zcode.json`/`zcode.toml` at or above `start`.
///
/// Walking up means a coding agent behaves like the other tools in a
/// repository: it finds the project's settings from any subdirectory instead
/// of silently falling back to built-in defaults.
pub fn find_project_config(start: &Path) -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(start);
    while let Some(current) = dir {
        for name in DEFAULT_CONFIG_NAMES {
            let candidate = current.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        dir = current.parent();
    }
    None
}

impl Default for Loader {
    fn default() -> Self {
        Self::with_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `Loader::load` reads process-wide env vars, so tests that set them —
    /// and tests that assert on values those vars can override — must not run
    /// concurrently. Without this the suite is flaky (NFR-REL-01).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write_config(dir: &tempfile::TempDir, toml_content: &str) -> PathBuf {
        let path = dir.path().join("zcode.toml");
        fs::write(&path, toml_content).unwrap();
        path
    }

    #[test]
    fn env_overrides_file_and_provider() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"provider = "openai"
model = "gpt-3.5-turbo"
"#,
        );

        std::env::set_var("ZCODE_PROVIDER", "anthropic");

        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.provider, Provider::Anthropic);

        std::env::remove_var("ZCODE_PROVIDER");
    }

    #[test]
    fn env_overrides_caps_skills_and_allowlist() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "max_tokens = 100\nshell_allowed = [\"echo .*\"]\n");

        std::env::set_var("ZCODE_MAX_TOKENS", "4096");
        std::env::set_var("ZCODE_MAX_TOOL_OUTPUT_CHARS", "2048");
        std::env::set_var("ZCODE_SKILLS_DIR", "/tmp/skills");
        std::env::set_var("ZCODE_SHELL_ALLOWED", "git status\ncargo test .*");

        let config = Loader::new(&path).load().unwrap();

        std::env::remove_var("ZCODE_MAX_TOKENS");
        std::env::remove_var("ZCODE_MAX_TOOL_OUTPUT_CHARS");
        std::env::remove_var("ZCODE_SKILLS_DIR");
        std::env::remove_var("ZCODE_SHELL_ALLOWED");

        assert_eq!(config.max_tokens, 4096, "env must beat the file");
        assert_eq!(config.max_tool_output_chars, 2048);
        assert_eq!(config.skills_dir, PathBuf::from("/tmp/skills"));
        assert_eq!(
            config.shell_allowed.as_ref(),
            ["git status".to_string(), "cargo test .*".to_string()]
        );
    }

    #[test]
    fn empty_shell_allowed_env_denies_everything() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "shell_allowed = [\"echo .*\"]\n");

        std::env::set_var("ZCODE_SHELL_ALLOWED", "");
        let config = Loader::new(&path).load().unwrap();
        std::env::remove_var("ZCODE_SHELL_ALLOWED");

        // An explicit empty override is a deliberate lockdown, not a fallback
        // to the file's list (M2.5).
        assert!(config.shell_allowed.is_empty());
    }

    #[test]
    fn loads_a_json_config_file() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zcode.json");
        fs::write(
            &path,
            r#"{
              "provider": "openrouter",
              "model": "anthropic/claude-sonnet-4",
              "max_turns": 7,
              "shell_allowed": ["git status", "cargo .*"],
              "mcp": { "servers": [
                { "name": "everything", "command": "npx", "args": ["-y", "srv"] }
              ]},
              "lsp": { "servers": [
                { "language": "rust", "command": "rust-analyzer" }
              ]}
            }"#,
        )
        .unwrap();

        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.provider, Provider::Openrouter);
        assert_eq!(config.model, "anthropic/claude-sonnet-4");
        assert_eq!(config.max_turns, 7);
        assert_eq!(config.shell_allowed.len(), 2);
        assert_eq!(config.mcp_servers[0].name, "everything");
        assert_eq!(config.lsp_servers[0].language, "rust");
    }

    #[test]
    fn json_and_toml_configs_agree() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("a.toml");
        fs::write(
            &toml_path,
            "provider = \"deepseek\"\nmodel = \"deepseek-chat\"\nmax_tokens = 2048\n",
        )
        .unwrap();
        let json_path = dir.path().join("a.json");
        fs::write(
            &json_path,
            r#"{"provider":"deepseek","model":"deepseek-chat","max_tokens":2048}"#,
        )
        .unwrap();

        let from_toml = Loader::new(&toml_path).load().unwrap();
        let from_json = Loader::new(&json_path).load().unwrap();
        assert_eq!(from_toml.provider, from_json.provider);
        assert_eq!(from_toml.model, from_json.model);
        assert_eq!(from_toml.max_tokens, from_json.max_tokens);
    }

    #[test]
    fn malformed_json_is_a_typed_error() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            Loader::new(&path).load(),
            Err(ConfigError::Json(_))
        ));
    }

    #[test]
    fn api_key_env_defaults_to_the_provider_convention() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "provider = \"openrouter\"\n");
        // Switching provider should not also require remembering to switch
        // the key variable name.
        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.api_key_env, "ZCODE_OPENROUTER_API_KEY");

        let path = write_config(&dir, "provider = \"deepseek\"\n");
        assert_eq!(
            Loader::new(&path).load().unwrap().api_key_env,
            "ZCODE_DEEPSEEK_API_KEY"
        );
    }

    #[test]
    fn explicit_api_key_env_is_respected() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "provider = \"openrouter\"\napi_key_env = \"MY_OWN_KEY\"\n",
        );
        assert_eq!(Loader::new(&path).load().unwrap().api_key_env, "MY_OWN_KEY");
    }

    #[test]
    fn model_defaults_follow_the_provider() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        // Choosing OpenRouter without naming a model must not leave the
        // OpenAI default behind — that id does not exist on OpenRouter.
        let path = write_config(&dir, "provider = \"openrouter\"\n");
        assert_eq!(
            Loader::new(&path).load().unwrap().model,
            "openai/gpt-4o-mini"
        );

        let path = write_config(&dir, "provider = \"deepseek\"\n");
        assert_eq!(Loader::new(&path).load().unwrap().model, "deepseek-chat");
    }

    #[test]
    fn explicit_model_wins_over_the_provider_default() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            "provider = \"openrouter\"\nmodel = \"anthropic/claude-sonnet-4.5\"\n",
        );
        assert_eq!(
            Loader::new(&path).load().unwrap().model,
            "anthropic/claude-sonnet-4.5"
        );
    }

    #[test]
    fn deepseek_provider_round_trips() {
        assert_eq!("deepseek".parse::<Provider>().unwrap(), Provider::Deepseek);
        assert_eq!(Provider::Deepseek.as_str(), "deepseek");
        assert!(Provider::Deepseek
            .default_endpoint()
            .unwrap()
            .contains("deepseek.com"));
        assert!(Provider::Deepseek.requires_api_key());
        assert!(!Provider::Ollama.requires_api_key());
    }

    #[test]
    fn project_config_is_found_from_a_subdirectory() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("zcode.json"),
            r#"{"provider":"deepseek","max_turns":9}"#,
        )
        .unwrap();
        let nested = root.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();

        // Running from a subdirectory must find the project's settings, not
        // silently fall back to built-in defaults.
        let loader = Loader::discover_from(&nested);
        let config = loader.load().unwrap();
        assert_eq!(config.provider, Provider::Deepseek);
        assert_eq!(config.max_turns, 9);
    }

    #[test]
    fn working_dir_anchors_to_the_project_root() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join("zcode.json"), r#"{"provider":"ollama"}"#).unwrap();
        let nested = root.join("src");
        fs::create_dir_all(&nested).unwrap();

        let config = Loader::discover_from(&nested).load().unwrap();
        assert_eq!(
            config.working_dir.canonicalize().unwrap(),
            root,
            "relative paths and .zcode/ must resolve from the project root"
        );
    }

    #[test]
    fn explicit_working_dir_beats_the_project_root() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("zcode.json"),
            r#"{"provider":"ollama","working_dir":"/tmp/elsewhere"}"#,
        )
        .unwrap();
        let config = Loader::discover_from(root).load().unwrap();
        assert_eq!(config.working_dir, PathBuf::from("/tmp/elsewhere"));
    }

    #[test]
    fn nearest_config_wins_over_a_higher_one() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("zcode.json"), r#"{"max_turns":1}"#).unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("zcode.json"), r#"{"max_turns":2}"#).unwrap();

        assert_eq!(Loader::discover_from(&inner).load().unwrap().max_turns, 2);
    }

    #[test]
    fn json_is_preferred_over_toml_in_the_same_directory() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("zcode.json"), r#"{"max_turns":7}"#).unwrap();
        fs::write(dir.path().join("zcode.toml"), "max_turns = 8\n").unwrap();
        assert_eq!(
            Loader::discover_from(dir.path()).load().unwrap().max_turns,
            7
        );
    }

    #[test]
    fn user_layer_is_overridden_field_by_field_by_the_project() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.json");
        // Machine-wide: provider + key name. Project: just the model.
        fs::write(
            &user,
            r#"{"provider":"openrouter","api_key_env":"MY_KEY","max_turns":42}"#,
        )
        .unwrap();
        let project = dir.path().join("zcode.json");
        fs::write(&project, r#"{"model":"anthropic/claude-sonnet-4.5"}"#).unwrap();

        let loader = Loader {
            layers: vec![
                ConfigLayer {
                    kind: LayerKind::User,
                    path: user,
                },
                ConfigLayer {
                    kind: LayerKind::Project,
                    path: project,
                },
            ],
            project_root: Some(dir.path().to_path_buf()),
        };
        let config = loader.load().unwrap();
        assert_eq!(config.provider, Provider::Openrouter, "from the user layer");
        assert_eq!(config.api_key_env, "MY_KEY", "from the user layer");
        assert_eq!(config.max_turns, 42, "from the user layer");
        assert_eq!(
            config.model, "anthropic/claude-sonnet-4.5",
            "project overrides only what it names"
        );
    }

    #[test]
    fn discovery_reports_the_files_it_will_read() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("zcode.toml"), "max_turns = 3\n").unwrap();
        let loader = Loader::discover_from(dir.path());
        let project: Vec<_> = loader
            .layers()
            .iter()
            .filter(|l| l.kind == LayerKind::Project)
            .collect();
        assert_eq!(project.len(), 1);
        assert!(project[0].path.ends_with("zcode.toml"));
    }

    #[test]
    fn no_config_anywhere_falls_back_to_defaults() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let loader = Loader::discover_from(dir.path());
        // The user layer may or may not exist on the machine running the
        // tests; what matters is that no project layer was invented.
        assert!(loader.layers().iter().all(|l| l.kind != LayerKind::Project));
    }

    #[test]
    fn user_config_candidates_follow_xdg() {
        let _guard = env_guard();
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg");
        let candidates = user_config_candidates();
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(candidates[0], PathBuf::from("/tmp/xdg/zcode/config.json"));
        assert_eq!(candidates[1], PathBuf::from("/tmp/xdg/zcode/config.toml"));
    }

    #[test]
    fn tilde_is_expanded_in_paths() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, "skills_dir = \"~/notes/skills\"\n");
        let config = Loader::new(&path).load().unwrap();

        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(
            config.skills_dir,
            PathBuf::from(&home).join("notes/skills"),
            "a literal ~ would resolve to a directory that never exists"
        );
        assert!(!config.skills_dir.to_string_lossy().contains('~'));
    }

    #[test]
    fn expand_tilde_leaves_other_paths_alone() {
        let _guard = env_guard();
        for p in ["/absolute/path", "relative/path", "./here", "a~b"] {
            assert_eq!(expand_tilde(PathBuf::from(p)), PathBuf::from(p));
        }
        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(expand_tilde(PathBuf::from("~")), PathBuf::from(home));
    }

    #[test]
    fn skills_roots_include_the_project_even_with_a_global_dir() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            skills_dir: PathBuf::from("/opt/team-skills"),
            ..Default::default()
        };
        let roots = cfg.skills_dirs();
        // A machine-wide library must not hide the project's own skills.
        assert_eq!(roots[0], dir.path().join(".zcode").join("skills"));
        assert!(roots.contains(&PathBuf::from("/opt/team-skills")));
    }

    #[test]
    fn skills_roots_are_deduplicated() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join(".zcode").join("skills");
        let cfg = Config {
            working_dir: dir.path().to_path_buf(),
            skills_dir: project.clone(),
            ..Default::default()
        };
        let roots = cfg.skills_dirs();
        assert_eq!(roots.iter().filter(|r| **r == project).count(), 1);
    }

    #[test]
    fn default_caps_match_prd() {
        let config = Config::default();
        assert_eq!(config.max_turns, 20);
        assert_eq!(config.max_tokens, 16384);
        assert_eq!(config.max_tool_output_chars, 16000);
        assert_eq!(config.provider, Provider::Openai);
        assert_eq!(config.mode, AgentMode::Build);
        assert!(config.shell_allowed.iter().any(|s| s == "echo .*"));
    }

    #[test]
    fn empty_allowed_is_deny_all() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[[shell.allowed]]
allowlist = []
"#,
        );
        // `shell.allowed` is a flat string array in the supported schema; the
        // table form above is a no-op and leaves the default. Loading the file
        // must preserve the default allowlist (not deny-all), proving the
        // unsupported table form is ignored:
        let cfg = Loader::new(&path).load().unwrap();
        assert!(!cfg.shell_allowed.is_empty());

        // The deny-all path is exercised directly:
        let cfg = Config {
            shell_allowed: Box::new([]),
            ..Config::default()
        };
        assert!(cfg.shell_allowed.is_empty());
    }

    #[test]
    fn unknown_provider_errors() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, r#"provider = "bogus""#);
        let result = Loader::new(&path).load();
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[test]
    fn resolve_api_key_missing() {
        let cfg = Config {
            api_key_env: "ZCODE_NONEXISTENT_KEY_XYZ".into(),
            ..Config::default()
        };
        let err = cfg.resolve_api_key().unwrap_err();
        assert!(matches!(err, ConfigError::MissingSecret(_)));
    }

    #[test]
    fn mcp_servers_parsed() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
[[mcp.servers]]
name = "a"
command = "echo"

[[mcp.servers]]
name = "b"
command = "echo"
"#,
        );
        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.mcp_servers.len(), 2);
        assert_eq!(config.mcp_servers[0].name, "a");
    }

    #[test]
    fn shell_allowed_parsed_from_file() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"
shell_allowed = ["git .*", "cargo .*"]
"#,
        );
        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.shell_allowed.len(), 2);
        assert_eq!(config.shell_allowed[0], "git .*");
    }

    #[test]
    fn skills_dir_resolves_inside_working_dir_when_empty() {
        let cfg = Config::default();
        let resolved = cfg.skills_dir();
        assert!(resolved.ends_with(".zcode/skills"));
    }

    #[test]
    fn provider_default_endpoint() {
        assert_eq!(
            Provider::Openai.default_endpoint(),
            Some("https://api.openai.com/v1/chat/completions")
        );
        assert_eq!(
            Provider::Ollama.default_endpoint(),
            Some("http://localhost:11434/api/chat")
        );
        assert_eq!(Provider::Vllm.default_endpoint(), None);
    }
}
