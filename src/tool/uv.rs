//! uv — fast Python package manager.

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

/// Detected via `uv.lock`.
pub fn detect(dir: &Path) -> bool {
    dir.join("uv.lock").exists()
}

/// `uv sync [--frozen]`
pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("uv");
    c.arg("sync");
    if frozen {
        c.arg("--frozen");
    }
    c
}

/// `uv run <args...>`
pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("uv");
    c.arg("run").args(args);
    c
}
