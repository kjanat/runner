//! Yarn — Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `yarn.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

/// `yarn <task> [args...]` (yarn infers `run`).
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg(task).args(args);
    c
}

/// `yarn install [--frozen-lockfile]`
///
/// Uses the classic flag for compatibility with Yarn 1 lockfile semantics.
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("yarn");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

/// `yarn exec <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg("exec").args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::install_cmd;

    #[test]
    fn frozen_install_uses_classic_compatible_flag() {
        let args: Vec<_> = install_cmd(true)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--frozen-lockfile"]);
    }
}
