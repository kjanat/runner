//! pnpm — fast, disk-efficient Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `pnpm-lock.yaml`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("pnpm-lock.yaml").exists()
}

/// `pnpm run <task> [-- args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("pnpm");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// `pnpm install [--frozen-lockfile] [--ignore-scripts]`
///
/// `--ignore-scripts` is appended when `deny_scripts`; it force-skips
/// dependency build scripts even on pnpm 10+, which otherwise consults the
/// `onlyBuiltDependencies` manifest allowlist.
pub(crate) fn install_cmd(frozen: bool, deny_scripts: bool) -> Command {
    let mut c = super::program::command("pnpm");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    if deny_scripts {
        c.arg("--ignore-scripts");
    }
    c
}

/// `pnpm exec <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("pnpm");
    c.arg("exec").args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::install_cmd;

    #[test]
    fn plain_install_has_no_extra_flags() {
        let args: Vec<_> = install_cmd(false, false)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install"]);
    }

    #[test]
    fn deny_scripts_appends_ignore_scripts() {
        let args: Vec<_> = install_cmd(false, true)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--ignore-scripts"]);
    }

    #[test]
    fn frozen_and_deny_scripts_combine() {
        let args: Vec<_> = install_cmd(true, true)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--frozen-lockfile", "--ignore-scripts"]);
    }
}
