use std::path::Path;
use std::process::Command;

pub const CLEAN_DIRS: &[&str] = &["vendor"];

pub fn detect(dir: &Path) -> bool {
    dir.join("go.mod").exists()
}

pub fn install_cmd() -> Command {
    let mut c = Command::new("go");
    c.arg("mod").arg("download");
    c
}
