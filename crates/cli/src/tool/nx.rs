//! Nx, monorepo build system.

use std::path::Path;

/// Detected via `nx.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("nx.json").exists()
}
