//! npm, the default Node.js package manager.

use std::path::Path;
use std::process::Command;

use super::HostVerbosity;

pub(crate) const fn quiet_capabilities() -> super::HostQuietCapabilities {
    super::HostQuietCapabilities::quiet("npm", &["--silent"])
}

/// Detected via `package-lock.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("package-lock.json").exists()
}

/// `npm [--silent] run <task> [-- args...]`
///
/// runner's `-q` maps to npm's `--silent` (`--loglevel=silent`), while npm's
/// own `-q`/`--quiet` maps to `--loglevel=warn`. npm 11 writes lifecycle banners
/// to stdout; npm 12, via `@npmcli/run-script` 11, emits them as notice logs on
/// stderr. `--silent` suppresses both forms so a `-q` pipeline stays clean. npm
/// has no stdout-diversion primitive, so
/// [`HostVerbosity::diverts_to_stderr`] is a no-op here.
pub(crate) fn run_cmd(task: &str, args: &[String], verbosity: HostVerbosity) -> Command {
    let mut c = super::program::command("npm");
    if verbosity.silences() {
        c.arg("--silent");
    }
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// `npx --package <package> -- <bin> [args...]`
pub(crate) fn exec_package_cmd(package: &str, bin: &str, args: &[String]) -> Command {
    let mut c = super::program::command("npx");
    c.arg("--package")
        .arg(package)
        .arg("--")
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
    fn run_without_quiet_has_no_silent_flag() {
        assert_eq!(
            args_of(&run_cmd("build", &[], HostVerbosity::default())),
            ["run", "build"]
        );
    }

    #[test]
    fn run_quiet_prepends_silent_before_run() {
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Quiet,
            stream: Stream::Inherit,
        };
        assert_eq!(
            args_of(&run_cmd("build", &["--flag".into()], v)),
            ["--silent", "run", "build", "--", "--flag"]
        );
    }

    #[test]
    fn run_stderr_stream_is_noop_for_npm() {
        // npm has no stdout-diversion primitive; the stream axis no-ops.
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Normal,
            stream: Stream::Stderr,
        };
        assert_eq!(args_of(&run_cmd("build", &[], v)), ["run", "build"]);
    }
}
