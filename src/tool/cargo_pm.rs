//! Cargo — the Rust package manager and build tool.

use std::path::Path;
use std::process::Command;

/// Directories produced by Cargo builds.
pub(crate) const CLEAN_DIRS: &[&str] = &["target"];

/// Detected via `Cargo.toml`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
}

/// Returns `true` if `Cargo.toml` contains a top-level `[workspace]` table.
///
/// Uses a line-anchored check to avoid false positives from
/// `[workspace.dependencies]` or comments.
pub(crate) fn detect_workspace(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|c| {
        c.lines().any(|line| {
            line.split('#')
                .next()
                .is_some_and(|part| part.trim() == "[workspace]")
        })
    })
}

/// `cargo build` or `cargo fetch` when `frozen`.
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("cargo");
    c.arg(if frozen { "fetch" } else { "build" });
    c
}

/// `cargo <args...>` — pass-through to Cargo subcommands.
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("cargo");
    c.args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::detect_workspace;

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("runner-{prefix}-{id}"));
            fs::create_dir(&path).expect("temp dir should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn workspace_detection_allows_inline_comments() {
        let dir = TempDir::new("cargo-workspace");

        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace] # root\nmembers = []\n",
        )
        .expect("Cargo.toml should be written");

        assert!(detect_workspace(dir.path()));
    }
}
