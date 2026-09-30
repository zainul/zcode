//! Spill store for over-budget tool output (FR-READ-07, CE-DQ12).
//!
//! When a tool result is cut to fit its budget, the full text is written here
//! and the cut result names the path — so the model can `grep` or `read` the
//! part it needs instead of re-running the command. Files live under
//! `<working_dir>/.zcode/spill/<session>/<call-id>.txt`; directories older
//! than the TTL are pruned when the store is created.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use domain::{BoxError, SpillPort};

use crate::StdFs;

pub struct SpillStore {
    working_dir: PathBuf,
    dir: PathBuf,
    fs: StdFs,
}

/// Keep ids to a safe, bounded file name: a call id comes from the provider.
fn sanitise(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unnamed".into()
    } else {
        cleaned
    }
}

impl SpillStore {
    /// A store under `<working_dir>/.zcode/spill`, pruning session
    /// directories not modified for `ttl_days`. Pruning failures are logged,
    /// never fatal: a stale spill file costs disk, not correctness.
    pub fn new(working_dir: &Path, ttl_days: u32) -> Self {
        let dir = working_dir.join(".zcode").join("spill");
        let store = Self {
            working_dir: working_dir.to_path_buf(),
            dir,
            fs: StdFs::new(),
        };
        store.prune(Duration::from_secs(u64::from(ttl_days) * 86_400));
        store
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn prune(&self, ttl: Duration) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > ttl);
            if old && entry.file_type().is_ok_and(|t| t.is_dir()) {
                if let Err(e) = fs::remove_dir_all(entry.path()) {
                    log::warn!("could not prune spill dir {}: {e}", entry.path().display());
                }
            }
        }
    }

    fn write(&self, session: &str, call_id: &str, content: &str) -> io::Result<String> {
        let path = self
            .dir
            .join(sanitise(session))
            .join(format!("{}.txt", sanitise(call_id)));
        self.fs.write_atomic(&path, content)?;
        let rel = path.strip_prefix(&self.working_dir).unwrap_or(&path);
        Ok(rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"))
    }
}

impl SpillPort for SpillStore {
    fn spill(&mut self, session: &str, call_id: &str, content: &str) -> Result<String, BoxError> {
        Ok(self.write(session, call_id, content)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spill_writes_the_full_text_and_returns_a_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SpillStore::new(dir.path(), 7);
        let path = store.spill("sess-1", "call_7", "all of it").unwrap();
        assert_eq!(path, ".zcode/spill/sess-1/call_7.txt");
        assert_eq!(
            fs::read_to_string(dir.path().join(&path)).unwrap(),
            "all of it"
        );
    }

    #[test]
    fn ids_are_sanitised_to_safe_file_names() {
        assert_eq!(sanitise("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitise(""), "unnamed");
        assert_eq!(sanitise(&"x".repeat(200)).len(), 64);
    }

    #[test]
    fn directories_past_the_ttl_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let spill = dir.path().join(".zcode/spill");
        fs::create_dir_all(spill.join("old-session")).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // A zero-day TTL: anything modified before now has expired.
        let _ = SpillStore::new(dir.path(), 0);
        assert!(!spill.join("old-session").exists());
    }

    #[test]
    fn directories_within_the_ttl_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let spill = dir.path().join(".zcode/spill");
        fs::create_dir_all(spill.join("recent-session")).unwrap();
        let _ = SpillStore::new(dir.path(), 7);
        assert!(spill.join("recent-session").exists());
    }
}
