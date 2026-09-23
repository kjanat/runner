//! Bun, all-in-one JavaScript runtime, bundler, and package manager.

use std::path::Path;
use std::process::Command;

pub(crate) const fn quiet_capabilities() -> super::HostQuietCapabilities {
    super::HostQuietCapabilities::quiet("bun", &["--silent"])
}

/// Detected via `bun.lockb` (binary) or `bun.lock` (text).
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("bun.lockb").exists() || dir.join("bun.lock").exists()
}

/// `bun run <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String], verbosity: super::HostVerbosity) -> Command {
    run_cmd_with_runtime(task, args, verbosity, false)
}

/// `bun [--bun] run <task> [args...]`
///
/// `--bun` symlinks `node` to bun for the script's process tree, so a
/// dependency bin carrying a `#!/usr/bin/env node` shebang also runs on bun
/// instead of the system Node. It goes before `run`: as a subcommand flag it
/// would be forwarded to the script.
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

/// `bun x --package <package> <bin> [args...]`
pub(crate) fn exec_package_cmd(package: &str, bin: &str, args: &[String]) -> Command {
    exec_package_cmd_with_runtime(package, bin, args, false)
}

/// `bun x [--bun] --package <package> <bin> [args...]`; `--bun` as in
/// [`exec_cmd_with_runtime`].
pub(crate) fn exec_package_cmd_with_runtime(
    package: &str,
    bin: &str,
    args: &[String],
    force_bun_runtime: bool,
) -> Command {
    let mut c = super::program::command("bun");
    c.arg("x");
    if force_bun_runtime {
        c.arg("--bun");
    }
    c.arg("--package").arg(package).arg(bin).args(args);
    c
}

/// `bun <file> [args...]`, execute a local script file with the Bun
/// runtime. Distinct from [`exec_cmd`] (`bun x`), which fetches and runs a
/// remote package; this runs an on-disk path the caller already resolved.
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command("bun");
    c.arg(file).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{run_cmd, run_file_cmd};

    fn assert_bun_program(cmd: &std::process::Command) {
        let stem = Path::new(cmd.get_program())
            .file_stem()
            .expect("bun command should have a file stem")
            .to_string_lossy();
        assert!(
            stem.eq_ignore_ascii_case("bun"),
            "expected bun executable, got {:?}",
            cmd.get_program()
        );
    }

    #[test]
    fn run_cmd_uses_bun_run() {
        let built: Vec<_> = run_cmd("lint", &[], crate::tool::HostVerbosity::default())
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "lint"]);
    }

    #[test]
    fn run_file_cmd_runs_the_path_directly() {
        let args = [String::from("--flag")];
        let cmd = run_file_cmd(Path::new("/abs/script.ts"), &args);
        let built: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_bun_program(&cmd);
        assert_eq!(built, ["/abs/script.ts", "--flag"]);
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
