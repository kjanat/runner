//! Yarn — Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `yarn.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

/// `yarn <task> [args...]` (yarn infers `run`).
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg(task).args(args);
    c
}

/// `yarn install [--immutable]` (Berry-compatible frozen flag).
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("yarn");
    c.arg("install");
    if frozen {
        c.arg("--immutable");
    }
    c
}

/// `yarn exec <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg("exec").args(args);
    c
}
