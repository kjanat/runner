use std::path::Path;
use std::process::Command;

pub fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg(task).args(args);
    c
}

pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("yarn");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg("exec").args(args);
    c
}
