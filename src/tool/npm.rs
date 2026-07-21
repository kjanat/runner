//! npm, the default Node.js package manager.

use std::path::Path;
use std::process::Command;

use super::{HostVerbosity, ScriptDirective};

/// Detected via `package-lock.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("package-lock.json").exists()
}

/// `npm [--silent] run <task> [-- args...]`
///
/// `--silent` (an alias for `--loglevel=silent`) is npm's own quiet switch; it
/// removes the lifecycle banner npm otherwise writes to **stdout**, so a `-q`
/// pipeline reading that stdout stays clean. npm has no stdout-diversion
/// primitive, so [`HostVerbosity::diverts_to_stderr`] is a no-op here.
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

/// `npm install` or `npm ci` when `frozen`.
///
/// [`ScriptDirective::Deny`] appends `--ignore-scripts` (skips both root and
/// dependency lifecycle scripts); [`ScriptDirective::ForceOn`] appends
/// `--no-ignore-scripts`, the nopt negation of the `ignore-scripts` boolean
/// config (same mechanism as `--no-save`/`--no-audit`), so scripts run even
/// if a future npm flips `ignore-scripts` on by default.
pub(crate) fn install_cmd(frozen: bool, scripts: ScriptDirective) -> Command {
    let mut c = super::program::command("npm");
    c.arg(if frozen { "ci" } else { "install" });
    match scripts {
        ScriptDirective::Deny => {
            c.arg("--ignore-scripts");
        }
        ScriptDirective::ForceOn => {
            c.arg("--no-ignore-scripts");
        }
        ScriptDirective::Default => {}
    }
    c
}

/// `npx <args...>`
///
/// Uses the standalone `npx` entrypoint for npm 6 compatibility, where
/// `npm exec` is unavailable.
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("npx");
    c.args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::{HostVerbosity, ScriptDirective, exec_cmd, install_cmd, run_cmd};
    use crate::tool::{QuietLevel, Stream};

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
            level: QuietLevel::Quiet,
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
            level: QuietLevel::Off,
            stream: Stream::Stderr,
        };
        assert_eq!(args_of(&run_cmd("build", &[], v)), ["run", "build"]);
    }

    #[test]
    fn frozen_install_uses_ci() {
        assert_eq!(
            args_of(&install_cmd(true, ScriptDirective::Default)),
            ["ci"]
        );
    }

    #[test]
    fn non_frozen_install_uses_install() {
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
    fn force_on_appends_no_ignore_scripts() {
        // The nopt negation of the `ignore-scripts` boolean config, keeps
        // scripts running even if npm flips the default to off.
        assert_eq!(
            args_of(&install_cmd(false, ScriptDirective::ForceOn)),
            ["install", "--no-ignore-scripts"]
        );
    }

    #[test]
    fn frozen_and_deny_scripts_combine() {
        assert_eq!(
            args_of(&install_cmd(true, ScriptDirective::Deny)),
            ["ci", "--ignore-scripts"]
        );
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
