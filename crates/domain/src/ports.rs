use std::path::{Path, PathBuf};

use crate::{Plugin, ShellCommand};

/// Stream type returned by LLM completions. Resolved to a boxed iterator over
/// result-chunks in infra; kept abstract here so Domain is async-agnostic.
pub struct CompletionChunk {
    pub delta: String,
    pub done: bool,
}

pub trait LlmPort {
    fn send(
        &mut self,
        system: &str,
        prompt: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;

    fn stream<'a>(
        &'a mut self,
        system: &'a str,
        prompt: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<CompletionChunk, Box<dyn std::error::Error + Send + Sync>>> + 'a,
    >;
}

pub trait FileSystemPort {
    fn read(&self, path: &Path) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
    fn write(
        &self,
        path: &Path,
        content: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn list(&self, path: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error + Send + Sync>>;
    fn exists(&self, path: &Path) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
    fn watch(
        &self,
        _path: &Path,
    ) -> Result<Box<dyn std::error::Error + Send + Sync>, Box<dyn std::error::Error + Send + Sync>>;
}

pub trait ShellPort {
    fn spawn(&mut self, cmd: &ShellCommand)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn run(
        &mut self,
        cmd: &ShellCommand,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

pub trait PluginRegistryPort {
    fn discover(&self) -> Result<Vec<Plugin>, Box<dyn std::error::Error + Send + Sync>>;
    fn load(&self, plugin: &Plugin) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    fn execute(
        &self,
        plugin: &Plugin,
        input: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

pub trait LoggerPort {
    fn log(&self, level: LogLevel, msg: &str);
    fn with_field(&self, key: &str, value: &str) -> Box<dyn LoggerPort + Send + Sync>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_chunk_done_semantics() {
        let chunk = CompletionChunk {
            delta: String::new(),
            done: true,
        };
        assert!(chunk.done);
        assert!(chunk.delta.is_empty());
    }
}
