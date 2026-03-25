//! Cargo — the Rust package manager and build tool.

use std::path::Path;
use std::process::Command;

/// Directories produced by Cargo builds.
pub const CLEAN_DIRS: &[&str] = &["target"];

/// Detected via `Cargo.toml`.
pub fn detect(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
}

/// Returns `true` if `Cargo.toml` contains a top-level `[workspace]` table.
///
/// Uses a line-anchored check to avoid false positives from
/// `[workspace.dependencies]` or comments.
pub fn detect_workspace(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml"))
        .is_ok_and(|c| c.lines().any(|l| l.trim() == "[workspace]"))
}

/// `cargo build` or `cargo fetch` when `frozen`.
pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("cargo");
    c.arg(if frozen { "fetch" } else { "build" });
    c
}

/// `cargo <args...>` — pass-through to Cargo subcommands.
pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("cargo");
    c.args(args);
    c
}
