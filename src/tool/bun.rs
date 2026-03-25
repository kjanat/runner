//! Bun — all-in-one JavaScript runtime, bundler, and package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `bun.lockb` (binary) or `bun.lock` (text).
pub fn detect(dir: &Path) -> bool {
    dir.join("bun.lockb").exists() || dir.join("bun.lock").exists()
}

/// `bun run <task> [args...]`
pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("bun");
    c.arg("run").arg(task).args(args);
    c
}

/// `bun install [--frozen-lockfile]`
pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("bun");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

/// `bunx <args...>`
pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("bunx");
    c.args(args);
    c
}
