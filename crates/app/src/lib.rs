//! Application layer — use-case orchestration over Domain ports.
//! Depends on `domain` only (FR-DI-02); no concrete infra crates.

use std::sync::Arc;

use domain::{
    AgentContext, FileEdit, FileSystemPort, LlmPort, LoggerPort, PluginRegistryPort, ShellPort,
};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("port resolution failed: {0}")]
    Port(String),
    #[error("{0}")]
    Domain(#[from] domain::DomainError),
}

/// A use-case trait. Concrete orchestration ships next milestone; here we
/// declare the contract so the composition root can wire it later.
pub trait TaskRunner {
    fn run(&self, ctx: &AgentContext, task: &domain::Task) -> Result<(), AppError>;
}

/// A use-case trait for planning edits against domain `FileEdit` values.
pub trait EditPlanner {
    fn plan(&self, ctx: &AgentContext, edit: &FileEdit) -> Result<(), AppError>;
}

/// Orchestrator holding boxed port trait-objects.
#[allow(dead_code)]
pub struct App<const N: usize = 4> {
    llm: Arc<dyn LlmPort + Send + Sync>,
    fs: Arc<dyn FileSystemPort + Send + Sync>,
    shell: Arc<dyn ShellPort + Send + Sync>,
    plugins: Arc<dyn PluginRegistryPort + Send + Sync>,
    logger: Arc<dyn LoggerPort + Send + Sync>,
}

impl App {
    pub fn new(
        llm: Arc<dyn LlmPort + Send + Sync>,
        fs: Arc<dyn FileSystemPort + Send + Sync>,
        shell: Arc<dyn ShellPort + Send + Sync>,
        plugins: Arc<dyn PluginRegistryPort + Send + Sync>,
        logger: Arc<dyn LoggerPort + Send + Sync>,
    ) -> Self {
        Self {
            llm,
            fs,
            shell,
            plugins,
            logger,
        }
    }
}

impl TaskRunner for App {
    fn run(&self, _ctx: &AgentContext, _task: &domain::Task) -> Result<(), AppError> {
        Err(AppError::Port(
            "task engine not implemented in v0.1.0".into(),
        ))
    }
}

impl EditPlanner for App {
    fn plan(&self, _ctx: &AgentContext, _edit: &FileEdit) -> Result<(), AppError> {
        Err(AppError::Port(
            "edit planner not implemented in v0.1.0".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopLlm;
    impl LlmPort for NoopLlm {
        fn send(
            &mut self,
            _system: &str,
            _prompt: &str,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
        fn stream<'a>(
            &'a mut self,
            _system: &'a str,
            _prompt: &'a str,
        ) -> Box<
            dyn Iterator<
                    Item = Result<
                        domain::ports::CompletionChunk,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + 'a,
        > {
            Box::new(std::iter::empty())
        }
    }

    struct NoopFs;
    impl FileSystemPort for NoopFs {
        fn read(
            &self,
            _path: &std::path::Path,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
        fn write(
            &self,
            _path: &std::path::Path,
            _content: &str,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        fn list(
            &self,
            _path: &std::path::Path,
        ) -> Result<Vec<std::path::PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
            Ok(Vec::new())
        }
        fn exists(
            &self,
            _path: &std::path::Path,
        ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
            Ok(false)
        }
        fn watch(
            &self,
            _path: &std::path::Path,
        ) -> Result<
            Box<dyn std::error::Error + Send + Sync>,
            Box<dyn std::error::Error + Send + Sync>,
        > {
            Ok(Box::new(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "stub",
            )))
        }
    }

    struct NoopShell;
    impl ShellPort for NoopShell {
        fn spawn(
            &mut self,
            _cmd: &domain::ShellCommand,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        fn run(
            &mut self,
            _cmd: &domain::ShellCommand,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(String::new())
        }
    }

    struct NoopPlugins;
    impl PluginRegistryPort for NoopPlugins {
        fn discover(
            &self,
        ) -> Result<Vec<domain::Plugin>, Box<dyn std::error::Error + Send + Sync>> {
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

    struct NoopLogger;
    impl LoggerPort for NoopLogger {
        fn log(&self, _level: domain::ports::LogLevel, _msg: &str) {}
        fn with_field(&self, _key: &str, _value: &str) -> Box<dyn LoggerPort + Send + Sync> {
            Box::new(NoopLogger)
        }
    }

    #[test]
    fn app_returns_port_error_for_run() {
        let app = App::new(
            Arc::new(NoopLlm),
            Arc::new(NoopFs),
            Arc::new(NoopShell),
            Arc::new(NoopPlugins),
            Arc::new(NoopLogger),
        );
        let ctx = AgentContext {
            working_dir: std::path::PathBuf::new(),
            model: "test".into(),
            env: Vec::new(),
        };
        let task = domain::Task {
            id: "t1".into(),
            description: "d".into(),
            status: domain::TaskStatus::Pending,
            constraints: Box::new([]),
        };
        let result = app.run(&ctx, &task);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Port(_)));
    }

    #[test]
    fn app_returns_port_error_for_plan() {
        let app = App::new(
            Arc::new(NoopLlm),
            Arc::new(NoopFs),
            Arc::new(NoopShell),
            Arc::new(NoopPlugins),
            Arc::new(NoopLogger),
        );
        let ctx = AgentContext {
            working_dir: std::path::PathBuf::new(),
            model: "test".into(),
            env: Vec::new(),
        };
        let edit = FileEdit {
            path: std::path::PathBuf::new(),
            old_content: String::new(),
            new_content: String::new(),
        };
        let result = app.plan(&ctx, &edit);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AppError::Port(_)));
    }
}
