//! pnpm, fast, disk-efficient Node.js package manager.

use std::path::Path;
use std::process::Command;

use super::HostVerbosity;

pub(crate) const fn quiet_capabilities() -> super::HostQuietCapabilities {
    super::HostQuietCapabilities::quiet("pnpm", &["--silent"]).with_stderr_diversion()
}

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

/// `pnpm --package=<package> dlx <bin> [args...]`
pub(crate) fn exec_package_cmd(package: &str, bin: &str, args: &[String]) -> Command {
    let mut c = super::program::command("pnpm");
    c.arg(format!("--package={package}"))
        .arg("dlx")
        .arg(bin)
        .args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::{HostVerbosity, run_cmd};
    use crate::tool::{HostDiagnostics, Stream};

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
            diagnostics: HostDiagnostics::Quiet,
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
            diagnostics: HostDiagnostics::Normal,
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
            diagnostics: HostDiagnostics::Reduced,
            stream: Stream::Stderr,
        };
        assert_eq!(
            args_of(&run_cmd("build", &[], v)),
            ["--silent", "--use-stderr", "run", "build"]
        );
    }
}
