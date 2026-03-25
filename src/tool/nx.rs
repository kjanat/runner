//! Nx — monorepo build system.

use std::path::Path;

/// Directories produced by Nx.
pub const CLEAN_DIRS: &[&str] = &[".nx"];

/// Detected via `nx.json`.
pub fn detect(dir: &Path) -> bool {
    dir.join("nx.json").exists()
}
