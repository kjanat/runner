//! Shared test-only helpers for tool module unit tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// Temporary directory wrapper removed on drop.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a uniquely named temp directory with the given `prefix`.
    pub(crate) fn new(prefix: &str) -> Self {
        let pid = std::process::id();

        for _ in 0..1024 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("runner-{prefix}-{pid}-{id}"));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => panic!("temp dir should be created: {e}"),
            }
        }

        panic!("temp dir should be created")
    }

    /// Borrow the temporary directory path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
