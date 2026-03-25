use std::path::Path;
use std::process::Command;

pub fn detect(dir: &Path) -> bool {
    dir.join("pnpm-lock.yaml").exists()
}

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("pnpm");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

pub fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("pnpm");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("pnpm");
    c.arg("exec").args(args);
    c
}
