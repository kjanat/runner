use std::path::Path;
use std::process::Command;

pub const CLEAN_DIRS: &[&str] = &[".venv", "__pycache__", ".mypy_cache", ".ruff_cache"];

pub fn detect(dir: &Path) -> bool {
    dir.join("Pipfile").exists() || dir.join("Pipfile.lock").exists()
}

pub fn install_cmd() -> Command {
    let mut c = Command::new("pipenv");
    c.arg("install");
    c
}
