//! Composer — the PHP dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `composer.json`.
pub fn detect(dir: &Path) -> bool {
    dir.join("composer.json").exists()
}

/// `composer install`
pub fn install_cmd() -> Command {
    let mut c = Command::new("composer");
    c.arg("install");
    c
}
