//! Bun — all-in-one JavaScript runtime, bundler, and package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `bun.lockb` (binary) or `bun.lock` (text).
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("bun.lockb").exists() || dir.join("bun.lock").exists()
}

/// `bun run <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("bun");
    c.arg("run").arg(task).args(args);
    c
}

/// `bun test [args...]`
pub(crate) fn test_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("bun");
    c.arg("test").args(args);
    c
}

/// `bun install [--frozen-lockfile]`
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = super::program::command("bun");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    c
}

/// `bunx <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("bunx");
    c.args(args);
    c
}

/// `bun <file> [args...]` — execute a local script file with the Bun
/// runtime. Distinct from [`exec_cmd`] (`bunx`), which fetches and runs a
/// remote package; this runs an on-disk path the caller already resolved.
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command("bun");
    c.arg(file).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{run_cmd, run_file_cmd, test_cmd};

    #[test]
    fn run_cmd_uses_bun_run() {
        let built: Vec<_> = run_cmd("lint", &[])
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "lint"]);
    }

    #[test]
    fn test_cmd_uses_bun_test() {
        let args = [String::from("--watch")];
        let built: Vec<_> = test_cmd(&args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["test", "--watch"]);
    }

    #[test]
    fn run_file_cmd_runs_the_path_directly() {
        let args = [String::from("--flag")];
        let cmd = run_file_cmd(Path::new("/abs/script.ts"), &args);
        let built: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), "bun");
        assert_eq!(built, ["/abs/script.ts", "--flag"]);
    }
}
