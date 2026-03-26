//! npm — the default Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `package-lock.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("package-lock.json").exists()
}

/// `npm run <task> [-- args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("npm");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// `npm install` or `npm ci` when `frozen`.
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = Command::new("npm");
    c.arg(if frozen { "ci" } else { "install" });
    c
}

/// `npx <args...>`
///
/// Uses the standalone `npx` entrypoint for npm 6 compatibility, where
/// `npm exec` is unavailable.
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("npx");
    c.args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::{exec_cmd, install_cmd};

    #[test]
    fn frozen_install_uses_ci() {
        let args: Vec<_> = install_cmd(true)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["ci"]);
    }

    #[test]
    fn non_frozen_install_uses_install() {
        let args: Vec<_> = install_cmd(false)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install"]);
    }

    #[test]
    fn exec_uses_npx_passthrough() {
        let args = [String::from("eslint"), String::from("--fix")];
        let built: Vec<_> = exec_cmd(&args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["eslint", "--fix"]);
    }
}
