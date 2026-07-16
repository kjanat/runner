//! pnpm, fast, disk-efficient Node.js package manager.

use std::path::Path;
use std::process::Command;

use super::ScriptDirective;

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
/// [`ScriptDirective::Deny`] appends `--ignore-scripts`; it force-skips
/// dependency build scripts even on pnpm 10+, which otherwise consults the
/// `onlyBuiltDependencies` manifest allowlist. [`ScriptDirective::ForceOn`]
/// adds nothing: pnpm 10+ denies dependency build scripts by default and only
/// the `onlyBuiltDependencies` manifest allowlist re-enables them, which runner
/// won't write, so `cmd::install` warns instead of emitting a misleading flag.
pub(crate) fn install_cmd(frozen: bool, scripts: ScriptDirective) -> Command {
    let mut c = super::program::command("pnpm");
    c.arg("install");
    if frozen {
        c.arg("--frozen-lockfile");
    }
    if scripts == ScriptDirective::Deny {
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
    use super::{ScriptDirective, install_cmd};

    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plain_install_has_no_extra_flags() {
        assert_eq!(
            args_of(&install_cmd(false, ScriptDirective::Default)),
            ["install"]
        );
    }

    #[test]
    fn deny_scripts_appends_ignore_scripts() {
        assert_eq!(
            args_of(&install_cmd(false, ScriptDirective::Deny)),
            ["install", "--ignore-scripts"]
        );
    }

    #[test]
    fn force_on_adds_no_flag() {
        // pnpm 10+ gates dependency build scripts behind the
        // `onlyBuiltDependencies` allowlist runner won't write, so force-on is
        // not flag-expressible; `cmd::install` warns about it instead.
        assert_eq!(
            args_of(&install_cmd(false, ScriptDirective::ForceOn)),
            ["install"]
        );
    }

    #[test]
    fn frozen_and_deny_scripts_combine() {
        assert_eq!(
            args_of(&install_cmd(true, ScriptDirective::Deny)),
            ["install", "--frozen-lockfile", "--ignore-scripts"]
        );
    }
}
