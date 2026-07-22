//! pnpm, fast, disk-efficient Node.js package manager.

use std::path::Path;
use std::process::Command;

use super::{HostVerbosity, ScriptDirective};

/// Detected via `pnpm-lock.yaml`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("pnpm-lock.yaml").exists()
}

/// `pnpm [--silent] [--use-stderr] run <task> [-- args...]`
///
/// `--silent` suppresses pnpm's own reporter output. pnpm is the one host that
/// can also honor the stream axis: `--use-stderr` ("Divert all output to
/// stderr") keeps **stdout** clean for the task alone, the exact primitive a
/// machine-readable pipeline wants, so [`HostVerbosity::diverts_to_stderr`]
/// appends it.
pub(crate) fn run_cmd(task: &str, args: &[String], verbosity: HostVerbosity) -> Command {
    let mut c = super::program::command("pnpm");
    if verbosity.silences() {
        c.arg("--silent");
    }
    if verbosity.diverts_to_stderr() {
        c.arg("--use-stderr");
    }
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
    use super::{HostVerbosity, ScriptDirective, install_cmd, run_cmd};
    use crate::tool::{QuietLevel, Stream};

    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_plain_has_no_flags() {
        assert_eq!(
            args_of(&run_cmd("build", &[], HostVerbosity::default())),
            ["run", "build"]
        );
    }

    #[test]
    fn run_quiet_prepends_silent() {
        let v = HostVerbosity {
            level: QuietLevel::Quiet,
            stream: Stream::Inherit,
        };
        assert_eq!(
            args_of(&run_cmd("build", &[], v)),
            ["--silent", "run", "build"]
        );
    }

    #[test]
    fn run_stderr_stream_appends_use_stderr() {
        let v = HostVerbosity {
            level: QuietLevel::Off,
            stream: Stream::Stderr,
        };
        assert_eq!(
            args_of(&run_cmd("build", &[], v)),
            ["--use-stderr", "run", "build"]
        );
    }

    #[test]
    fn run_quiet_and_stderr_combine() {
        let v = HostVerbosity {
            level: QuietLevel::Silent,
            stream: Stream::Stderr,
        };
        assert_eq!(
            args_of(&run_cmd("build", &[], v)),
            ["--silent", "--use-stderr", "run", "build"]
        );
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
