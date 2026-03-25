//! pnpm — fast, disk-efficient Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `pnpm-lock.yaml`.
pub fn detect(dir: &Path) -> bool {
    dir.join("pnpm-lock.yaml").exists()
}

/// `pnpm run <task> [-- args...]`
pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("pnpm");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// `pnpm install [--frozen-lockfile]`
pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("pnpm");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

/// `pnpm exec <args...>`
pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("pnpm");
    c.arg("exec").args(args);
    c
}
