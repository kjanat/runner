//! Go modules — the Go dependency system.

use std::path::Path;
use std::process::Command;

/// Directories that may be cleaned in a Go project.
pub const CLEAN_DIRS: &[&str] = &["vendor"];

/// Detected via `go.mod`.
pub fn detect(dir: &Path) -> bool {
    dir.join("go.mod").exists()
}

/// `go mod download`
pub fn install_cmd() -> Command {
    let mut c = Command::new("go");
    c.arg("mod").arg("download");
    c
}
