use std::path::Path;
use std::process::Command;

pub fn detect(dir: &Path) -> bool {
    dir.join("Gemfile").exists()
}

pub fn install_cmd() -> Command {
    let mut c = Command::new("bundle");
    c.arg("install");
    c
}
