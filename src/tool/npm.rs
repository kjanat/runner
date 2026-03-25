//! npm — the default Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `package-lock.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("package-lock.json").exists()
}

/// `npm run <task> [-- args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("npm");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// `npm install` or `npm ci` when `frozen`.
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("npm");
    c.arg(if frozen { "ci" } else { "install" });
    c
}

/// `npx <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("npx");
    c.args(args);
    c
}
