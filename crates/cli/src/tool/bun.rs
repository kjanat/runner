//! Bun, all-in-one JavaScript runtime, bundler, and package manager.

use std::path::Path;
#[cfg(test)]
use std::process::Command;

/// Detected via `bun.lockb` (binary) or `bun.lock` (text).
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("bun.lockb").exists() || dir.join("bun.lock").exists()
}

/// `bun run <task> [args...]`
#[cfg(test)]
pub(crate) fn run_cmd(task: &str, args: &[String], verbosity: super::HostVerbosity) -> Command {
    run_cmd_with_runtime(task, args, verbosity, false)
}

/// `bun [--bun] run <task> [args...]`
///
/// `--bun` symlinks `node` to bun for the script's process tree, so a
/// dependency bin carrying a `#!/usr/bin/env node` shebang also runs on bun
/// instead of the system Node. It goes before `run`: as a subcommand flag it
/// would be forwarded to the script.
#[cfg(test)]
pub(crate) fn run_cmd_with_runtime(
    task: &str,
    args: &[String],
    verbosity: super::HostVerbosity,
    force_bun_runtime: bool,
) -> Command {
    let mut c = super::program::command("bun");
    if force_bun_runtime {
        c.arg("--bun");
    }
    c.arg("run");
    // `bun run --silent` skips the command echo bun prints before the script.
    // bun has no stdout-diversion primitive, so the stream axis no-ops.
    if verbosity.silences() {
        c.arg("--silent");
    }
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {

    use super::run_cmd;

    #[test]
    fn run_cmd_uses_bun_run() {
        let built: Vec<_> = run_cmd("lint", &[], crate::tool::HostVerbosity::default())
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "lint"]);
    }
}

#[cfg(test)]
mod verbosity_tests {
    use super::run_cmd;
    use super::run_cmd_with_runtime;
    use crate::tool::{HostDiagnostics, HostVerbosity};

    fn argv(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_cmd_default_adds_no_verbosity_flag() {
        let v = HostVerbosity::default();
        assert_eq!(argv(&run_cmd("build", &[], v)), ["run", "build"]);
    }

    #[test]
    fn force_bun_runtime_puts_the_flag_before_run() {
        // `bun --bun run x`, not `bun run --bun x`: after the subcommand the
        // flag is forwarded to the script instead of switching the runtime.
        let v = HostVerbosity::default();
        assert_eq!(
            argv(&run_cmd_with_runtime("build", &[], v, true)),
            ["--bun", "run", "build"]
        );
    }

    #[test]
    fn force_bun_runtime_composes_with_quiet() {
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Quiet,
            ..HostVerbosity::default()
        };
        assert_eq!(
            argv(&run_cmd_with_runtime("build", &[], v, true)),
            ["--bun", "run", "--silent", "build"]
        );
    }

    #[test]
    fn run_cmd_is_the_unforced_form() {
        let v = HostVerbosity::default();
        assert_eq!(
            argv(&run_cmd("build", &[], v)),
            argv(&run_cmd_with_runtime("build", &[], v, false))
        );
    }

    #[test]
    fn run_cmd_quiet_maps_to_host_flag() {
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Quiet,
            ..HostVerbosity::default()
        };
        assert_eq!(
            argv(&run_cmd("build", &[], v)),
            ["run", "--silent", "build"]
        );
    }
}
