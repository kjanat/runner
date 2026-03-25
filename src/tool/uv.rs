use std::path::Path;
use std::process::Command;

pub const CLEAN_DIRS: &[&str] = &[".venv", "__pycache__", ".mypy_cache", ".ruff_cache"];

pub fn detect(dir: &Path) -> bool {
    dir.join("uv.lock").exists()
}

pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("uv");
    c.arg("sync");
    if frozen {
        c.arg("--frozen");
    }
    c
}

pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("uv");
    c.arg("run").args(args);
    c
}
