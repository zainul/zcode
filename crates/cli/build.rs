use std::process::Command;

/// Build script that emits `VERGEN_GIX_SHA` and `VERGEN_BUILD_PROFILE` env vars.
///
/// This replaces the `vergen-gix` crate, which requires Rust >= 1.88.
/// The API surface matches what the CLI reads at compile time:
/// `env!("VERGEN_GIX_SHA", "unknown")` and `env!("VERGEN_BUILD_PROFILE", "unknown")`.
fn main() {
    // Re-run only when these files change.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=build.rs");

    // Git SHA: try `git rev-parse HEAD`, fall back to reading .git/HEAD directly.
    let git_sha = git_sha().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=VERGEN_GIX_SHA={}", git_sha);

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
