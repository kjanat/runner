//! uv — fast Python package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `uv.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("uv.lock").exists()
}

/// `uv sync [--frozen]`
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("uv");
    c.arg("sync");
    if frozen {
        c.arg("--frozen");
    }
    c
}

/// `uv run <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("uv");
    c.arg("run").args(args);
    c
}
