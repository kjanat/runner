//! Nx — monorepo build system.

use std::path::Path;

/// Directories produced by Nx.
pub(crate) const CLEAN_DIRS: &[&str] = &[".nx"];

/// Detected via `nx.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("nx.json").exists()
}
