//! Bundler — the Ruby dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `Gemfile`.
pub fn detect(dir: &Path) -> bool {
    dir.join("Gemfile").exists()
}

/// `bundle install`
pub fn install_cmd() -> Command {
    let mut c = Command::new("bundle");
    c.arg("install");
    c
}
