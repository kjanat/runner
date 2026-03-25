use std::path::Path;
use std::process::Command;

pub const CLEAN_DIRS: &[&str] = &["target"];

pub fn detect(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
}

pub fn detect_workspace(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|c| c.contains("[workspace]"))
}

pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("cargo");
    c.arg(if frozen { "fetch" } else { "build" });
    c
}

pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("cargo");
    c.args(args);
    c
}
