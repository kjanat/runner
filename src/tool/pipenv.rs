//! Pipenv — Python dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `Pipfile` or `Pipfile.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("Pipfile").exists() || dir.join("Pipfile.lock").exists()
}

/// `pipenv install`
pub(crate) fn install_cmd() -> Command {
    let mut c = Command::new("pipenv");
    c.arg("install");
    c
}
