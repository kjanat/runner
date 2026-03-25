use std::path::Path;
use std::process::Command;

pub fn detect(dir: &Path) -> bool {
    dir.join("composer.json").exists()
}

pub fn install_cmd() -> Command {
    let mut c = Command::new("composer");
    c.arg("install");
    c
}
