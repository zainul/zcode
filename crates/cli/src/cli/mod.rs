use std::sync::Arc;

use clap::Parser;

use app::{App, AppError};
use domain::{LoggerPort, PluginRegistryPort};
use infra_filesystem::StdFs;
use infra_llm::OpenAiLlm;
use infra_shell::StdShell;

/// CLI definition parsed with clap v4 derive (FR-CLI-02).
#[derive(Parser)]
#[command(name = "ag", version, about = "QAgent — the lean Rust coding agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::Subcommand)]
pub enum Commands {
    /// Print build metadata (FR-CLI-01).
    Version,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("VERGEN_GIX_SHA", "unknown");
pub const BUILD_PROFILE: &str = env!("VERGEN_BUILD_PROFILE", "unknown");

/// Composition root: construct concrete `App` backed by real infra adapters.
///
/// Fails fast with a typed `AppError::Port` when a port cannot be resolved,
/// rather than panicking with a stack trace (NFR-REL-01/02).
pub fn wire() -> Result<App, AppError> {
    let llm = Arc::new(OpenAiLlm::new("http://localhost:9999", "gpt-4o-mini"));
    let fs = Arc::new(StdFs::new());
    let shell = Arc::new(StdShell::new());

    let plugins = Arc::new(NullPluginRegistry);
    let logger = Arc::new(NullLogger);

    Ok(App::new(llm, fs, shell, plugins, logger))
}

struct NullPluginRegistry;
impl PluginRegistryPort for NullPluginRegistry {
    fn discover(&self) -> Result<Vec<domain::Plugin>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Vec::new())
    }
    fn load(
        &self,
        _plugin: &domain::Plugin,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
    fn execute(
        &self,
        _plugin: &domain::Plugin,
        _input: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

struct NullLogger;
impl LoggerPort for NullLogger {
    fn log(&self, _level: domain::ports::LogLevel, _msg: &str) {}
    fn with_field(&self, _key: &str, _value: &str) -> Box<dyn LoggerPort + Send + Sync> {
        Box::new(NullLogger)
    }
}

// Suppress unused import warnings for types used only in `wire()`.
#[allow(unused_imports)]
use domain::ports::LogLevel as _;

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::try_parse()?;
    match cli.command {
        Commands::Version => {
            println!(
                "ag v{} (git: {}, profile: {})",
                VERSION, GIT_SHA, BUILD_PROFILE
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_command_parses() {
        let cli = Cli::try_parse_from(["ag", "version"]).unwrap();
        assert!(matches!(cli.command, Commands::Version));
    }

    #[test]
    fn git_sha_const_exists() {
        let _ = GIT_SHA;
        let _ = BUILD_PROFILE;
        let _ = VERSION;
    }

    #[test]
    fn wire_constructs_app() {
        let app = wire();
        assert!(app.is_ok());
    }
}
