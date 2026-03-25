//! mise — polyglot dev tool version manager.

use std::path::Path;

/// Detected via `mise.toml` or `.mise.toml`.
pub fn detect(dir: &Path) -> bool {
    dir.join("mise.toml").exists() || dir.join(".mise.toml").exists()
}
