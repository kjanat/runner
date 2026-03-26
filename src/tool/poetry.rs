//! Poetry — Python dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `poetry.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("poetry.lock").exists()
}

/// `poetry install`
pub(crate) fn install_cmd() -> Command {
    let mut c = Command::new("poetry");
    c.arg("install");
    c
}
