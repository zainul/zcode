//! Log routing.
//!
//! `env_logger` writes to stderr, which in the TUI is the same terminal the
//! alternate screen is painting on — a single `log::warn!` from a failing MCP
//! or LSP server would draw over the prompt box and stay there until the next
//! full redraw. That is not cosmetic: the warning is unreadable *and* the UI
//! is corrupted.
//!
//! So the process installs one logger that can be redirected. Outside the TUI
//! it behaves exactly like `env_logger`. While the TUI is up, records are
//! diverted to a channel and rendered in the tools pane as ordinary `!` lines,
//! where they are legible and bounded.

use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use log::{Level, Log, Metadata, Record};

/// Where diverted records go while the TUI owns the screen.
static SINK: OnceLock<Mutex<Option<Sender<String>>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<Sender<String>>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// A logger that writes to stderr until someone claims the terminal.
struct SwitchableLogger {
    inner: env_logger::Logger,
}

impl Log for SwitchableLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.inner.enabled(record.metadata()) {
            return;
        }
        // A poisoned lock must not lose the record or panic inside logging;
        // fall through to stderr, which is the pre-TUI behaviour.
        if let Ok(guard) = sink().lock() {
            if let Some(tx) = guard.as_ref() {
                let line = format!(
                    "[{}] {}",
                    record.level().as_str().to_lowercase(),
                    record.args()
                );
                // A closed channel means the TUI is gone; dropping is right.
                let _ = tx.send(line);
                return;
            }
        }
        self.inner.log(record);
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Install the process logger. Safe to call once; later calls are ignored.
pub fn init() {
    let inner =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).build();
    let level = inner.filter();
    if log::set_boxed_logger(Box::new(SwitchableLogger { inner })).is_ok() {
        log::set_max_level(level);
    }
}

/// Divert log records to `tx` until the returned guard is dropped.
///
/// The guard matters: if the TUI exits by any path — a quit, an error, a
/// panic — logging has to go back to stderr, or the rest of the process runs
/// silently.
pub struct LogRedirect;

impl LogRedirect {
    pub fn to(tx: Sender<String>) -> Self {
        if let Ok(mut guard) = sink().lock() {
            *guard = Some(tx);
        }
        Self
    }
}

impl Drop for LogRedirect {
    fn drop(&mut self) {
        if let Ok(mut guard) = sink().lock() {
            *guard = None;
        }
    }
}

/// Whether a level should be shown prominently in the UI.
pub fn is_loud(level: Level) -> bool {
    matches!(level, Level::Error | Level::Warn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn the_redirect_guard_restores_stderr_logging() {
        let (tx, _rx) = mpsc::channel();
        assert!(sink().lock().unwrap().is_none(), "starts undiverted");
        {
            let _guard = LogRedirect::to(tx);
            assert!(sink().lock().unwrap().is_some(), "diverted while held");
        }
        assert!(
            sink().lock().unwrap().is_none(),
            "a TUI that exits must not leave logging captured"
        );
    }

    #[test]
    fn warnings_and_errors_are_the_loud_ones() {
        assert!(is_loud(Level::Error));
        assert!(is_loud(Level::Warn));
        assert!(!is_loud(Level::Info));
        assert!(!is_loud(Level::Debug));
    }
}
