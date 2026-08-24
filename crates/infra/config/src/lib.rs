//! Configuration model + loader for QAgent.
//! Secrets are read from `AG_*` env vars only, never written to disk.

use domain::AgentContext;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub model: String,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            model: DEFAULT_MODEL.to_string(),
            working_dir,
            env: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl Config {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }

    pub fn to_agent_context(&self) -> AgentContext {
        AgentContext {
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
            let file_config: ConfigFile = toml::from_str(&content)?;
            if let Some(model) = file_config.model {
                config.model = model;
            }
            if let Some(working_dir) = file_config.working_dir {
                config.working_dir = working_dir;
            }
            if let Some(timeout_ms) = file_config.timeout_ms {
                config.timeout_ms = timeout_ms;
            }
            if let Some(env) = file_config.env {
                config.env = env;
            }
        }

        if let Ok(model) = std::env::var("AG_MODEL") {
            config.model = model;
        }
        if let Ok(working_dir) = std::env::var("AG_WORKING_DIR") {
            config.working_dir = PathBuf::from(working_dir);
        }
        if let Ok(t) = std::env::var("AG_TIMEOUT_MS") {
            if let Ok(ms) = t.parse::<u64>() {
                config.timeout_ms = ms;
            }
        }

        Ok(config)
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    working_dir: Option<PathBuf>,
    #[serde(default)]
    env: Option<Vec<(String, String)>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
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
    use tempfile::tempdir;

    #[test]
    fn default_config_has_conservative_values() {
        let config = Config::default();
        assert_eq!(config.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert!(!config.model.is_empty());
    }

    #[test]
    fn env_overrides_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("ag.toml");

        let toml_content = r#"
model = "gpt-3.5-turbo"
timeout_ms = 1000
"#;
        fs::write(&config_path, toml_content).unwrap();

        std::env::set_var("AG_MODEL", "gpt-4o");
        std::env::set_var("AG_TIMEOUT_MS", "99999");

        let loader = Loader::new(&config_path);
        let config = loader.load().unwrap();

        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.timeout_ms, 99999);

        std::env::remove_var("AG_MODEL");
        std::env::remove_var("AG_TIMEOUT_MS");
    }

    #[test]
    fn file_values_loaded_when_no_env() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("ag.toml");

        let toml_content = r#"
model = "gpt-3.5-turbo"
timeout_ms = 1000
"#;
        fs::write(&config_path, toml_content).unwrap();

        // Ensure env vars don't interfere
        std::env::remove_var("AG_MODEL");
        std::env::remove_var("AG_TIMEOUT_MS");

        let loader = Loader::new(&config_path);
        let config = loader.load().unwrap();

        assert_eq!(config.model, "gpt-3.5-turbo");
        assert_eq!(config.timeout_ms, 1000);
    }
}
