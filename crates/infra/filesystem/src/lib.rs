//! Filesystem adapter backed by `std::fs`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::{fs, io};

#[derive(thiserror::Error, Debug)]
pub enum FsError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub struct StdFs;

impl StdFs {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdFs {
    fn default() -> Self {
        Self
    }
}

impl domain::FileSystemPort for StdFs {
    fn read(&self, path: &Path) -> Result<String, Box<dyn Error + Send + Sync>> {
        let content = fs::read_to_string(path)?;
        Ok(content)
    }

    fn write(&self, path: &Path, content: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    fn list(&self, path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error + Send + Sync>> {
        let entries = fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        Ok(entries)
    }

    fn exists(&self, path: &Path) -> Result<bool, Box<dyn Error + Send + Sync>> {
        Ok(path.exists())
    }

    fn watch(
        &self,
        _path: &Path,
    ) -> Result<Box<dyn Error + Send + Sync>, Box<dyn Error + Send + Sync>> {
        Err(Box::new(FsError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem watch not implemented in v0.1.0",
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::FileSystemPort;
    use tempfile::tempdir;

    #[test]
    fn round_trip_write_read() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let fs = StdFs::new();

        fs.write(&file_path, "hello qagent").unwrap();
        let content = fs.read(&file_path).unwrap();
        assert_eq!(content, "hello qagent");
    }

    #[test]
    fn exists_reports_file_present() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("exists.txt");
        let fs = StdFs::new();

        fs.write(&file_path, "data").unwrap();
        assert!(fs.exists(&file_path).unwrap());
        assert!(!fs.exists(&dir.path().join("missing.txt")).unwrap());
    }

    #[test]
    fn list_returns_entries() {
        let dir = tempdir().unwrap();
        let fs = StdFs::new();

        fs.write(&dir.path().join("a.txt"), "a").unwrap();
        fs.write(&dir.path().join("b.txt"), "b").unwrap();

        let entries = fs.list(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn read_missing_returns_error() {
        let dir = tempdir().unwrap();
        let fs = StdFs::new();
        let missing = dir.path().join("nonexistent.txt");

        let result = fs.read(&missing);
        assert!(result.is_err());
    }

    #[test]
    fn watch_returns_error_stub() {
        let dir = tempdir().unwrap();
        let fs = StdFs::new();
        let result = fs.watch(&dir.path());
        assert!(result.is_err());
    }
}
