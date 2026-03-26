use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("runner-{prefix}-{id}"));
        fs::create_dir(&path).expect("temp dir should be created");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
