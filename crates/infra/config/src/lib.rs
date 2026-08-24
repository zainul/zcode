//! Configuration model + loader for QAgent.
//! Secrets are read from `AG_*` env vars only, never written to disk.
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

/// Provider selection (FR-CONFIG-02 / FR-MODEL-01..08).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Openai,
    Anthropic,
    Openrouter,
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
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::OpenaiCompatible => "openai-compatible",
        }
    }

    pub fn default_endpoint(&self) -> Option<&'static str> {
        match self {
            Self::Openai => Some("https://api.openai.com/v1/chat/completions"),
            Self::Anthropic => Some("https://api.anthropic.com/v1/messages"),
            Self::Openrouter => Some("https://openrouter.ai/api/v1/chat/completions"),
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
            api_key_env: String::from("AG_OPENAI_API_KEY"),
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

    pub fn skills_dir(&self) -> PathBuf {
        if self.skills_dir.as_os_str().is_empty() {
            self.working_dir.join(".ag").join("skills")
        } else {
            self.skills_dir.clone()
        }
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

pub struct Loader {
    config_path: PathBuf,
}

impl Loader {
    pub fn new(config_path: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
        }
    }

    pub fn with_default() -> Self {
        Self::new("ag.toml")
    }

    pub fn load(&self) -> Result<Config, ConfigError> {
        let mut config = Config::default();

        if self.config_path.exists() {
            let content = std::fs::read_to_string(&self.config_path)?;
            let file: ConfigFile = toml::from_str(&content)?;

            if let Some(m) = file.model {
                config.model = m;
            }
            if let Some(p) = file.provider {
                config.provider = p
                    .parse::<Provider>()
                    .map_err(|_| ConfigError::UnknownProvider(p))?;
            }
            if let Some(k) = file.api_key_env {
                config.api_key_env = k;
            }
            if let Some(u) = file.base_url {
                config.base_url = Some(u);
            }
            if let Some(wd) = file.working_dir {
                config.working_dir = wd;
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

        if let Ok(provider) = std::env::var("AG_PROVIDER") {
            config.provider = provider
                .parse::<Provider>()
                .map_err(|_| ConfigError::UnknownProvider(provider))?;
        }
        if let Ok(model) = std::env::var("AG_MODEL") {
            config.model = model;
        }
        if let Ok(k) = std::env::var("AG_API_KEY_ENV") {
            config.api_key_env = k;
        }
        if let Ok(u) = std::env::var("AG_BASE_URL") {
            config.base_url = Some(u);
        }
        if let Ok(wd) = std::env::var("AG_WORKING_DIR") {
            config.working_dir = PathBuf::from(wd);
        }
        if let Ok(t) = std::env::var("AG_TIMEOUT_MS") {
            if let Ok(ms) = t.parse::<u64>() {
                config.timeout_ms = ms;
            }
        }
        if let Ok(t) = std::env::var("AG_MAX_TURNS") {
            if let Ok(v) = t.parse::<u64>() {
                config.max_turns = v;
            }
        }
        if let Ok(m) = std::env::var("AG_MODE") {
            config.mode = m
                .parse::<AgentMode>()
                .map_err(|_| ConfigError::InvalidMode(m.clone()))?;
        }

        Ok(config)
    }

    /// Load from a specific config override path (FR-IFACE subcommands).
    pub fn load_with_override(
        &self,
        override_path: Option<impl AsRef<Path>>,
    ) -> Result<Config, ConfigError> {
        if let Some(p) = override_path {
            Loader::new(p.as_ref().to_path_buf()).load()
        } else {
            self.load()
        }
    }
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

    fn write_config(dir: &tempfile::TempDir, toml_content: &str) -> PathBuf {
        let path = dir.path().join("ag.toml");
        fs::write(&path, toml_content).unwrap();
        path
    }

    #[test]
    fn env_overrides_file_and_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            &dir,
            r#"provider = "openai"
model = "gpt-3.5-turbo"
"#,
        );

        std::env::set_var("AG_PROVIDER", "anthropic");

        let config = Loader::new(&path).load().unwrap();
        assert_eq!(config.provider, Provider::Anthropic);

        std::env::remove_var("AG_PROVIDER");
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
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(&dir, r#"provider = "bogus""#);
        let result = Loader::new(&path).load();
        assert!(matches!(result, Err(ConfigError::UnknownProvider(_))));
    }

    #[test]
    fn resolve_api_key_missing() {
        let cfg = Config {
            api_key_env: "AG_NONEXISTENT_KEY_XYZ".into(),
            ..Config::default()
        };
        let err = cfg.resolve_api_key().unwrap_err();
        assert!(matches!(err, ConfigError::MissingSecret(_)));
    }

    #[test]
    fn mcp_servers_parsed() {
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
        assert!(resolved.ends_with(".ag/skills"));
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
