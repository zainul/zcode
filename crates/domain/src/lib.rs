//! Domain layer — core business rules, entities, domain errors, and port traits.
//! Pure stdlib: zero third-party dependencies (enforced by FR-DI-01).

pub mod error;
pub mod model;
pub mod ports;

pub use error::DomainError;
pub use model::{AgentContext, FileEdit, Plugin, ShellCommand, Task, TaskStatus};
pub use ports::{FileSystemPort, LlmPort, LoggerPort, PluginRegistryPort, ShellPort};
