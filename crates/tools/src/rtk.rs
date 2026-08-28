//! Token-optimised shell output via [rtk](https://github.com/rtk-ai/rtk).
//!
//! rtk is a CLI proxy: it runs the command you asked for and filters what
//! comes back, so `git status` returns 647 bytes instead of 2295. For an agent
//! that feeds every tool result into a transcript, that is the difference
//! between output and *billable* output.
//!
//! zcode does not reimplement any of that. It asks rtk one question before
//! running a shell command — "is there a better spelling of this?" — via
//! `rtk rewrite`, which rtk documents as the single source of truth its own
//! hooks use. Deciding here which commands are safe to rewrite would mean
//! duplicating rtk's judgement and getting it wrong: `test -f x` must not
//! become `rtk test -f x`, `read` is a shell builtin, and `env FOO=1 make`
//! has to keep its prefix. rtk already knows all of that.
//!
//! The rewrite happens **after** the shell guard has passed the original
//! command, for two reasons. A user's `shell_allowed` patterns are written
//! against the commands they type, and would stop matching if every one grew
//! an `rtk ` prefix. And the denylist reads the command the *user* meant,
//! which is the one worth judging. The result is re-checked against the
//! denylist all the same — see [`Rtk::rewrite`].

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long to wait for `rtk rewrite`. It is one short-lived process on the
/// path of every shell call; rtk advertises <10ms, so a second is already
/// generous, and hanging the agent on a wedged proxy is not a trade worth
/// making.
const REWRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long to wait for a package manager. Installing is a one-off, but a
/// startup that never finishes is worse than one without rtk.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

/// A located rtk binary.
#[derive(Debug, Clone)]
pub struct Rtk {
    binary: PathBuf,
    version: String,
}

impl Rtk {
    /// Find rtk: the configured path if given, else the first on `PATH`.
    ///
    /// Returns `None` rather than an error — rtk is an optimisation, and a
    /// machine without it must behave exactly as it did before.
    pub fn detect(configured: Option<&str>) -> Option<Self> {
        let binary = match configured {
            Some(path) => {
                let path = PathBuf::from(path);
                path.exists().then_some(path)?
            }
            None => infra_config::which_on_path("rtk")?,
        };
        let version = probe_version(&binary)?;
        Some(Self { binary, version })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn path(&self) -> &Path {
        &self.binary
    }

    /// The rtk equivalent of `command`, or `None` if there is not one.
    ///
    /// Keyed on **stdout, not the exit code**. `rtk rewrite --help` promises
    /// `0` for a rewrite and `1` for none; rtk 0.36.0 actually exits `3` on a
    /// rewrite. Reading the output is right under either, and under whatever
    /// the next version decides.
    pub fn rewrite(&self, command: &str) -> Option<String> {
        if command.trim().is_empty() {
            return None;
        }
        let output = run(
            Command::new(&self.binary).arg("rewrite").arg(command),
            REWRITE_TIMEOUT,
        )
        .ok()?;
        let rewritten = String::from_utf8_lossy(&output).trim().to_string();
        // An empty answer, or one that changed nothing, is not a rewrite.
        // A multi-line answer is rtk telling us something else — a warning, a
        // prompt — and is not a command; running it would be guessing.
        if rewritten.is_empty() || rewritten == command.trim() || rewritten.contains('\n') {
            return None;
        }
        Some(rewritten)
    }
}

/// Package managers that can install rtk, in the order worth trying.
///
/// Homebrew only, deliberately. `rtk` is in homebrew-core rather than a
/// third-party tap, so what it installs is auditable. The alternatives are
/// not: `cargo install rtk` resolves to **an unrelated crate** (Rust Type
/// Kit), and the upstream shell installer is `curl … | sh` — the exact
/// pattern zcode's own denylist refuses. A tool that forbids the model from
/// piping the network into a shell must not do it itself.
const INSTALLERS: &[(&str, &[&str])] = &[("brew", &["install", "rtk"])];

/// What to tell someone who has no supported package manager.
pub const MANUAL_INSTALL_HINT: &str =
    "install rtk from https://github.com/rtk-ai/rtk (`brew install rtk`), or set \
     `rtk.enabled = false` to stop looking for it";

#[derive(Debug)]
pub enum InstallError {
    /// No package manager this can safely drive.
    NoInstaller,
    /// The package manager ran and failed.
    Failed { installer: String, reason: String },
    /// It reported success but rtk still is not there.
    NotOnPathAfterwards,
    /// An attempt failed recently; not retrying yet.
    RecentlyFailed,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoInstaller => write!(f, "no supported package manager — {MANUAL_INSTALL_HINT}"),
            Self::Failed { installer, reason } => write!(f, "{installer} failed: {reason}"),
            Self::NotOnPathAfterwards => {
                write!(f, "the installer reported success but rtk is not on PATH")
            }
            Self::RecentlyFailed => write!(
                f,
                "a previous install failed within the last day, so this run did not retry — \
                 {MANUAL_INSTALL_HINT}"
            ),
        }
    }
}

impl std::error::Error for InstallError {}

/// How long a failed auto-install is remembered before another is attempted.
///
/// Without this, a machine where the install cannot succeed — no network, a
/// broken formula — retries on *every single run*, each one costing however
/// long the package manager takes to fail. A day is long enough that nobody
/// notices the retries and short enough that fixing the cause is picked up
/// the same afternoon.
const INSTALL_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Where the last failed attempt is recorded.
fn attempt_marker() -> Option<PathBuf> {
    Some(infra_config::user_state_dir()?.join("rtk-install-failed"))
}

/// Whether an auto-install should be attempted, given where the marker lives.
///
/// Takes the path rather than reading the environment so it can be tested
/// against a scratch directory. A unit that reads `HOME` can only be tested by
/// moving `HOME`, which is process-global and breaks whatever else is running.
///
/// Cheap and best-effort: no marker, or an unreadable one, means yes — failing
/// to install rtk twice is a smaller problem than never installing it.
fn install_is_due_at(marker: Option<&Path>) -> bool {
    let Some(marker) = marker else {
        return true;
    };
    let Ok(modified) = std::fs::metadata(marker).and_then(|m| m.modified()) else {
        return true;
    };
    modified
        .elapsed()
        .map(|since| since >= INSTALL_RETRY_AFTER)
        .unwrap_or(true)
}

fn install_is_due() -> bool {
    install_is_due_at(attempt_marker().as_deref())
}

/// Remember that an attempt failed, so the next run does not repeat it.
fn record_failed_attempt_at(marker: Option<&Path>) {
    let Some(marker) = marker else {
        return;
    };
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(marker, b"");
}

/// Forget any recorded failure, so a fixed machine is not held back.
fn clear_failed_attempt_at(marker: Option<&Path>) {
    if let Some(marker) = marker {
        let _ = std::fs::remove_file(marker);
    }
}

/// Whether [`install`] would actually run a package manager on this call.
///
/// Lets the caller say "installing rtk now" *before* the wait starts, instead
/// of explaining a minute of silence after it ends.
pub fn install_will_be_attempted() -> bool {
    install_is_due()
        && INSTALLERS
            .iter()
            .any(|(name, _)| infra_config::which_on_path(name).is_some())
}

/// Install rtk with a package manager the machine already has.
///
/// Only ever runs a package manager that is *already present*: this installs
/// a package, it does not install a package manager, and it never fetches a
/// script to run.
///
/// A failure is remembered for [`INSTALL_RETRY_AFTER`]; see [`install_is_due`].
pub fn install() -> Result<Rtk, InstallError> {
    if !install_is_due() {
        return Err(InstallError::RecentlyFailed);
    }
    match install_now() {
        Ok(rtk) => {
            clear_failed_attempt_at(attempt_marker().as_deref());
            Ok(rtk)
        }
        Err(e) => {
            record_failed_attempt_at(attempt_marker().as_deref());
            Err(e)
        }
    }
}

fn install_now() -> Result<Rtk, InstallError> {
    let Some((installer, args)) = INSTALLERS
        .iter()
        .find(|(name, _)| infra_config::which_on_path(name).is_some())
    else {
        return Err(InstallError::NoInstaller);
    };
    log::info!("installing rtk with `{installer} {}`", args.join(" "));
    run(Command::new(installer).args(*args), INSTALL_TIMEOUT).map_err(|reason| {
        InstallError::Failed {
            installer: (*installer).to_string(),
            reason,
        }
    })?;
    Rtk::detect(None).ok_or(InstallError::NotOnPathAfterwards)
}

/// `rtk --version` → `0.36.0`.
fn probe_version(binary: &Path) -> Option<String> {
    let out = run(Command::new(binary).arg("--version"), REWRITE_TIMEOUT).ok()?;
    let text = String::from_utf8_lossy(&out);
    // "rtk 0.36.0" — take the version, or the whole line if it is shaped
    // differently in some future release.
    let line = text.lines().next()?.trim();
    Some(
        line.split_whitespace()
            .nth(1)
            .unwrap_or(line)
            .trim()
            .to_string(),
    )
}

/// Run a command to completion, with a timeout, returning stdout.
///
/// The polling loop is the same shape `infra-shell` uses, and for the same
/// reason: the ports are synchronous by design, so there is no runtime to
/// await on (see `CLAUDE.md`, "No async runtime, anywhere").
fn run(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().map_err(|e| e.to_string())?;
                // The caller judges the *output*: `rtk rewrite` exits non-zero
                // on success, so a status check here would reject every
                // rewrite it makes.
                if output.stdout.is_empty() && !status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let first = stderr.lines().next().unwrap_or("").trim();
                    if !first.is_empty() {
                        return Err(first.to_string());
                    }
                }
                return Ok(output.stdout);
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtk() -> Option<Rtk> {
        Rtk::detect(None)
    }

    #[test]
    fn a_failed_install_is_not_retried_on_every_single_run() {
        // Without the cooldown, a machine that cannot install rtk pays the
        // package manager's failure cost on every invocation, forever.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("rtk-install-failed");

        assert!(install_is_due_at(Some(&marker)), "nothing recorded yet");
        record_failed_attempt_at(Some(&marker));
        assert!(
            !install_is_due_at(Some(&marker)),
            "the failure is remembered"
        );
        clear_failed_attempt_at(Some(&marker));
        assert!(install_is_due_at(Some(&marker)), "and forgotten on success");
    }

    #[test]
    fn an_old_failure_stops_holding_the_machine_back() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("rtk-install-failed");
        record_failed_attempt_at(Some(&marker));

        // Backdate it past the window; a machine that has since been fixed
        // should try again rather than stay disabled forever.
        let long_ago = std::time::SystemTime::now() - INSTALL_RETRY_AFTER - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert!(install_is_due_at(Some(&marker)));
    }

    #[test]
    fn with_nowhere_to_record_it_would_rather_retry_than_give_up() {
        // Failing to install twice is a smaller problem than never installing.
        assert!(install_is_due_at(None));
        // And the recording calls are no-ops rather than panics.
        record_failed_attempt_at(None);
        clear_failed_attempt_at(None);
    }

    #[test]
    fn the_marker_is_machine_wide_not_per_project() {
        // Per-project would mean every repository retries a broken install
        // independently, which is the pathology the cooldown exists to stop.
        let Some(marker) = attempt_marker() else {
            return;
        };
        assert!(
            marker.starts_with(infra_config::user_state_dir().unwrap()),
            "{marker:?}"
        );
    }

    #[test]
    fn a_machine_without_rtk_reports_nothing_rather_than_failing() {
        // rtk is an optimisation; its absence is not an error condition.
        assert!(Rtk::detect(Some("/nonexistent/rtk")).is_none());
    }

    #[test]
    #[ignore = "requires rtk on PATH"]
    fn rewrites_the_commands_rtk_knows() {
        let Some(rtk) = rtk() else { return };
        assert_eq!(rtk.rewrite("git status").as_deref(), Some("rtk git status"));
        assert_eq!(rtk.rewrite("ls -la").as_deref(), Some("rtk ls -la"));
    }

    #[test]
    #[ignore = "requires rtk on PATH"]
    fn leaves_alone_what_rtk_has_no_equivalent_for() {
        // The cases that make a hand-written rewrite table wrong: `test` is a
        // condition evaluator, not a test runner; `read` is a shell builtin.
        let Some(rtk) = rtk() else { return };
        for command in ["echo hi", "test -f Cargo.toml", "read line", "rm -rf build"] {
            assert_eq!(rtk.rewrite(command), None, "{command}");
        }
    }

    #[test]
    #[ignore = "requires rtk on PATH"]
    fn an_empty_command_is_never_rewritten() {
        let Some(rtk) = rtk() else { return };
        assert_eq!(rtk.rewrite(""), None);
        assert_eq!(rtk.rewrite("   "), None);
    }

    #[test]
    #[ignore = "requires rtk on PATH"]
    fn a_rewrite_is_recognised_despite_the_exit_code() {
        // rtk 0.36.0 exits 3 on a successful rewrite, though its own --help
        // documents 0. Reading stdout is what makes this version-proof.
        let Some(rtk) = rtk() else { return };
        assert!(rtk.rewrite("cargo test").is_some());
    }

    #[test]
    #[ignore = "requires rtk on PATH"]
    fn the_version_is_a_version() {
        let Some(rtk) = rtk() else { return };
        let v = rtk.version();
        assert!(
            v.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "got {v:?}"
        );
    }
}
