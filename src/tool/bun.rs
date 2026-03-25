use std::path::Path;
use std::process::Command;

pub fn detect(dir: &Path) -> bool {
    dir.join("bun.lockb").exists() || dir.join("bun.lock").exists()
}

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("bun");
    c.arg("run").arg(task).args(args);
    c
}

pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("bun");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("bunx");
    c.args(args);
    c
}
