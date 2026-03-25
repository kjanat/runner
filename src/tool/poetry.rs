//! Poetry — Python dependency manager.

use std::path::Path;
use std::process::Command;

/// Common Python artifact directories.
pub(crate) const CLEAN_DIRS: &[&str] = &[
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
];

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
