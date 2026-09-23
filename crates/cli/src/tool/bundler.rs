//! Bundler, the Ruby dependency manager.

use std::path::Path;

/// Detected via `Gemfile`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("Gemfile").exists()
}
