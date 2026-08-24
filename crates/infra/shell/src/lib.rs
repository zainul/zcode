//! Shell adapter backed by `std::process::Command`.

use domain::ShellCommand;
use std::error::Error;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(thiserror::Error, Debug)]
pub enum ShellError {
    #[error("pty sessions deferred to v0.2")]
    Pty,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout after {0}ms")]
    Timeout(u64),
}

pub struct StdShell;

impl StdShell {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdShell {
    fn default() -> Self {
        Self
    }
}

impl domain::ShellPort for StdShell {
    fn spawn(&mut self, _cmd: &ShellCommand) -> Result<(), Box<dyn Error + Send + Sync>> {
        Err(Box::new(ShellError::Pty))
    }

    fn run(&mut self, cmd: &ShellCommand) -> Result<String, Box<dyn Error + Send + Sync>> {
        let (program, arg) = if cfg!(target_os = "windows") {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };

        let default_cwd = std::env::current_dir()?;
        let cwd = cmd.cwd.as_deref().unwrap_or(&default_cwd);

        let mut child = Command::new(program)
            .arg(arg)
            .arg(&cmd.command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(cwd)
            .spawn()?;

        let start = Instant::now();
        let timeout = Duration::from_millis(cmd.timeout_ms);

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let output = child.wait_with_output()?;
                    if !status.success() {
                        return Err(format!(
                            "command failed with status {}: {}",
                            status,
                            String::from_utf8_lossy(&output.stderr)
                        )
                        .into());
                    }
                    return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        return Err(Box::new(ShellError::Timeout(cmd.timeout_ms)));
                    }
                    // Small spin-wait to avoid busy-looping; production code
                    // would use a proper wait with timeout or tokio.
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(Box::new(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::ShellPort;

    #[test]
    fn run_echo_qagent_contains_output() {
        let mut shell = StdShell::new();
        let cmd = ShellCommand {
            command: "echo qagent".into(),
            cwd: None,
            env: Vec::new(),
            timeout_ms: 5_000,
        };
        let output = shell.run(&cmd).unwrap();
        assert!(output.contains("qagent"));
    }

    #[test]
    fn spawn_returns_pty_error() {
        let mut shell = StdShell::new();
        let cmd = ShellCommand {
            command: "true".into(),
            cwd: None,
            env: Vec::new(),
            timeout_ms: 1_000,
        };
        let result = shell.spawn(&cmd);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("deferred to v0.2"));
    }

    #[test]
    fn run_missing_command_returns_error() {
        let mut shell = StdShell::new();
        let cmd = ShellCommand {
            command: "this_command_does_not_exist_xyz".into(),
            cwd: None,
            env: Vec::new(),
            timeout_ms: 5_000,
        };
        let result = shell.run(&cmd);
        assert!(result.is_err());
    }
}
