//! Pipenv — Python dependency manager.

use std::path::Path;
use std::process::Command;

/// Common Python artifact directories.
pub const CLEAN_DIRS: &[&str] = &[
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
];

/// Detected via `Pipfile` or `Pipfile.lock`.
pub fn detect(dir: &Path) -> bool {
    dir.join("Pipfile").exists() || dir.join("Pipfile.lock").exists()
}

/// `pipenv install`
pub fn install_cmd() -> Command {
    let mut c = Command::new("pipenv");
    c.arg("install");
    c
}
