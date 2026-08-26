use std::process::Command;

/// Build script that stamps build metadata into the binary.
///
/// This replaces the `vergen-gix` crate, which requires Rust >= 1.88.
/// It emits `VERGEN_GIX_SHA`, `VERGEN_BUILD_PROFILE`, and `ZCODE_BUILD_TIME`.
///
/// The build time matters: between releases the crate version and commit stay
/// the same, so without it an installed binary and a freshly built one report
/// an identical version and there is no way to tell whether an update landed.
fn main() {
    // Re-run only when these files change.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");

    // Always re-run so the build timestamp tracks the actual build.
    println!("cargo:rerun-if-changed=.git/index");

    // Git SHA: try `git rev-parse HEAD`, fall back to reading .git/HEAD directly.
    // Abbreviated, with a marker when the working tree has uncommitted changes
    // — a "dirty" build is exactly the case where the commit alone misleads.
    let git_sha = git_sha().unwrap_or_else(|| "unknown".to_string());
    let short: String = git_sha.chars().take(7).collect();
    let sha_label = if working_tree_is_dirty() {
        format!("{short}-dirty")
    } else {
        short
    };
    println!("cargo:rustc-env=VERGEN_GIX_SHA={}", sha_label);

    println!("cargo:rustc-env=ZCODE_BUILD_TIME={}", build_time());

    // Build profile: debug by default, release from profile flag.
    let profile = if std::env::var("PROFILE")
        .map(|p| p == "release")
        .unwrap_or(false)
    {
        "release"
    } else {
        "debug"
    };
    println!("cargo:rustc-env=VERGEN_BUILD_PROFILE={}", profile);
}

fn git_sha() -> Option<String> {
    // Try `git rev-parse HEAD` in the workspace root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let workspace_root = Path::new(&manifest_dir)
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new(&manifest_dir));

    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(workspace_root)
        .output()
        .ok();

    if let Some(out) = output {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                return Some(sha);
            }
        }
    }

    // Fallback: read .git/HEAD directly.
    let git_head = workspace_root.join(".git/HEAD");
    if let Ok(content) = std::fs::read_to_string(&git_head) {
        let content = content.trim();
        if content.starts_with("ref: ") {
            let ref_path = workspace_root
                .join(".git")
                .join(content.strip_prefix("ref: ").unwrap());
            if let Ok(sha) = std::fs::read_to_string(&ref_path) {
                return Some(sha.trim().to_string());
            }
        } else if !content.is_empty() {
            return Some(content.to_string());
        }
    }

    None
}

use std::path::Path;

/// True when `git status --porcelain` reports anything.
fn working_tree_is_dirty() -> bool {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let root = Path::new(&manifest_dir)
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new(&manifest_dir));
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

/// UTC build timestamp, `YYYY-MM-DDTHH:MM:SSZ`, using pure calendar math so
/// the build script keeps its zero-dependency policy.
fn build_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
