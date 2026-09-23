//! Composer, the PHP dependency manager.

use std::path::Path;

/// Detected via `composer.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("composer.json").exists()
}
