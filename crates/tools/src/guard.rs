//! Shell command allowlist (FR-CONFIG-04/05, NFR-SEC-02).
//!
//! `GuardedShell` decorates `StdShell`: the allowlist is applied *before* the
//! command ever reaches `std::process::Command`, and the default is **deny**.

use std::error::Error;
use std::fmt;

use domain::{ShellCommand, ShellPort};
use infra_shell::StdShell;
use regex::Regex;

/// Characters that let a command reach beyond the text we can check:
/// substitution (`` ` ``, `$(`), redirection (`>`, `<`) and backgrounding /
/// chaining (`&`). A command containing any of them is refused outright —
/// otherwise `echo hi $(rm -rf /)` would pass an `echo .*` pattern.
const FORBIDDEN_SUBSTRINGS: &[&str] = &["`", "$(", "${", ">", "<", "&"];

#[derive(Debug)]
pub enum ShellToolError {
    /// The command did not satisfy the allowlist.
    Blocked(String),
    /// A pattern in `shell_allowed` is not a valid regex.
    BadPattern { pattern: String, reason: String },
}

impl fmt::Display for ShellToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked(cmd) => write!(
                f,
                "command blocked by the shell allowlist (`shell_allowed` in zcode.json/zcode.toml): {cmd}"
            ),
            Self::BadPattern { pattern, reason } => {
                write!(f, "invalid shell_allowed pattern {pattern:?}: {reason}")?;
                if let Some(hint) = pattern_hint(pattern) {
                    write!(f, "\n  hint: {hint}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for ShellToolError {}

/// Squeeze the regex crate's multi-line diagnostic into one line.
fn regex_reason(err: &regex::Error) -> String {
    err.to_string()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('^') && !l.starts_with("regex parse error"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Advice for the mistakes people actually make writing these patterns.
fn pattern_hint(pattern: &str) -> Option<String> {
    let trimmed = pattern.trim();
    if trimmed == "*" {
        return Some(
            "these are regular expressions, not shell globs — use \".*\" to allow every \
             command (which disables the safety net entirely)"
                .to_string(),
        );
    }
    if trimmed.starts_with('*') || trimmed.starts_with('+') || trimmed.starts_with('?') {
        return Some(format!(
            "a repetition operator needs something to repeat — did you mean \".{trimmed}\"?"
        ));
    }
    if trimmed.contains('*') && !trimmed.contains(".*") && !trimmed.contains("\\*") {
        return Some(
            "these are regular expressions, not shell globs — `.*` matches any text, \
             `*` on its own is invalid"
                .to_string(),
        );
    }
    None
}

/// `StdShell` wrapped in the configured allowlist.
pub struct GuardedShell {
    inner: StdShell,
    allowed: Box<[Regex]>,
}

impl GuardedShell {
    /// Compile the allowlist. Every pattern is anchored to match a **whole**
    /// command segment, so `ls .*` cannot be satisfied by an `ls` appearing
    /// anywhere inside a longer command.
    pub fn new(inner: StdShell, allowed: &[String]) -> Result<Self, ShellToolError> {
        let mut compiled = Vec::with_capacity(allowed.len());
        for pattern in allowed {
            let anchored = format!("^(?:{pattern})$");
            let re = Regex::new(&anchored).map_err(|e| ShellToolError::BadPattern {
                pattern: pattern.clone(),
                // The regex crate's message names the offending construct,
                // which is far more actionable than "invalid pattern".
                reason: regex_reason(&e),
            })?;
            compiled.push(re);
        }
        Ok(Self {
            inner,
            allowed: compiled.into_boxed_slice(),
        })
    }

    pub fn is_allowed(&self, command: &str) -> bool {
        is_allowed(command, &self.allowed)
    }

    /// Run `cmd` only if it satisfies the allowlist (FR-CONFIG-05).
    pub fn run_guarded(&mut self, cmd: &ShellCommand) -> Result<String, domain::BoxError> {
        if !self.is_allowed(&cmd.command) {
            return Err(Box::new(ShellToolError::Blocked(cmd.command.clone())));
        }
        self.inner.run(cmd)
    }
}

/// Split a command line into the individual commands a shell would run.
/// Splitting on `;` / `|` / newline only ever *adds* constraints (each part
/// must independently be allowed), so it cannot widen access.
fn segments(command: &str) -> Vec<&str> {
    command
        .split([';', '|', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// FR-CONFIG-04: a command runs iff every segment matches ≥1 allowed pattern.
/// An empty allowlist matches nothing, so it denies everything (M2.5).
pub fn is_allowed(command: &str, allowed: &[Regex]) -> bool {
    if allowed.is_empty() {
        return false;
    }
    if FORBIDDEN_SUBSTRINGS.iter().any(|s| command.contains(s)) {
        return false;
    }
    let parts = segments(command);
    if parts.is_empty() {
        return false;
    }
    parts
        .iter()
        .all(|part| allowed.iter().any(|re| re.is_match(part)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_allowed() -> Box<[Regex]> {
        ["echo .*", "ls .*", "cd .*", "cat .*"]
            .iter()
            .map(|p| Regex::new(&format!("^(?:{p})$")).unwrap())
            .collect()
    }

    #[test]
    fn allows_listed_command() {
        assert!(is_allowed("echo hi", &default_allowed()));
        assert!(is_allowed("ls crates", &default_allowed()));
    }

    #[test]
    fn blocks_rm_rf() {
        assert!(!is_allowed("rm -rf /", &default_allowed()));
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(!is_allowed("echo hi", &[]));
        assert!(!is_allowed("", &[]));
    }

    #[test]
    fn blocks_when_any_segment_is_not_allowed() {
        // The `echo` half is fine; the `rm` half is not — so the whole line is
        // refused (FR-CONFIG-04).
        assert!(!is_allowed("echo foo; rm -rf /", &default_allowed()));
        assert!(!is_allowed("ls /tmp | rm -rf /", &default_allowed()));
    }

    #[test]
    fn blocks_substitution_and_redirection() {
        // These would smuggle an unchecked command past an allowed prefix.
        assert!(!is_allowed("echo hi $(rm -rf /)", &default_allowed()));
        assert!(!is_allowed("echo `rm -rf /`", &default_allowed()));
        assert!(!is_allowed("echo hi > /etc/passwd", &default_allowed()));
        assert!(!is_allowed("echo hi && rm -rf /", &default_allowed()));
    }

    #[test]
    fn patterns_are_anchored_to_whole_segments() {
        // Without anchoring, `.is_match` would find "echo x" inside this.
        assert!(!is_allowed("sudo echo x", &default_allowed()));
    }

    #[test]
    fn guarded_shell_runs_allowed_and_blocks_denied() {
        let allow: Vec<String> = vec!["echo .*".into()];
        let mut sh = GuardedShell::new(StdShell::new(), &allow).unwrap();

        let ok = sh
            .run_guarded(&ShellCommand {
                command: "echo hi".into(),
                cwd: None,
                env: Vec::new(),
                timeout_ms: 5_000,
            })
            .expect("allowed command runs");
        assert!(ok.contains("hi"), "got {ok:?}");

        let err = sh
            .run_guarded(&ShellCommand {
                command: "rm -rf /".into(),
                cwd: None,
                env: Vec::new(),
                timeout_ms: 5_000,
            })
            .unwrap_err();
        assert!(err.to_string().contains("blocked"), "got {err}");
    }

    #[test]
    fn deny_all_when_config_has_empty_list() {
        let mut sh = GuardedShell::new(StdShell::new(), &[]).unwrap();
        assert!(sh
            .run_guarded(&ShellCommand {
                command: "echo hi".into(),
                cwd: None,
                env: Vec::new(),
                timeout_ms: 5_000,
            })
            .is_err());
    }

    #[test]
    fn a_bare_star_is_reported_as_a_glob_mistake() {
        // Writing "*" (shell-glob thinking) previously failed every run with
        // just "invalid shell_allowed pattern: *".
        let Err(err) = GuardedShell::new(StdShell::new(), &["*".to_string()]) else {
            panic!("`*` is not a valid regex");
        };
        let text = err.to_string();
        assert!(
            text.contains("regular expressions, not shell globs"),
            "{text}"
        );
        assert!(text.contains(".*"), "{text}");
    }

    #[test]
    fn leading_repetition_suggests_a_dot() {
        let Err(err) = GuardedShell::new(StdShell::new(), &["*.rs".to_string()]) else {
            panic!("`*.rs` is not a valid regex");
        };
        assert!(err.to_string().contains("did you mean"), "{err}");
    }

    #[test]
    fn invalid_pattern_is_reported() {
        let bad: Vec<String> = vec!["(unclosed".into()];
        assert!(matches!(
            GuardedShell::new(StdShell::new(), &bad),
            Err(ShellToolError::BadPattern { .. })
        ));
    }
}
