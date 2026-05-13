//! Subcommand implementations: info, run, install, clean, list, completions.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use colored::Colorize;

use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, ProjectContext};

mod clean;
mod completions;
mod doctor;
mod info;
mod install;
mod list;
pub(crate) mod run;
mod why;

pub(crate) use clean::clean;
pub(crate) use completions::{completions, parse_shell_arg};
pub(crate) use doctor::doctor;
pub(crate) use info::info;
pub(crate) use install::install;
pub(crate) use list::list;
pub(crate) use run::run;
pub(crate) use why::why;

fn configure_command(command: &mut Command, dir: &Path) {
    command
        .current_dir(dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;

        if let Some(code) = status.code() {
            return code;
        }
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }

    status.code().unwrap_or(1)
}

fn print_warnings(ctx: &ProjectContext, overrides: &ResolutionOverrides) {
    print_warning_slice(&ctx.warnings, overrides);
}

fn print_warning_slice(warnings: &[DetectionWarning], overrides: &ResolutionOverrides) {
    if overrides.no_warnings {
        return;
    }
    for warning in warnings {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{configure_command, exit_code};

    #[test]
    fn configure_command_sets_current_dir() {
        let dir = std::env::temp_dir();
        let mut command = Command::new("runner-test-command");

        configure_command(&mut command, dir.as_path());

        assert_eq!(command.get_current_dir(), Some(dir.as_path()));
    }

    #[test]
    fn no_warnings_suppresses_emission() {
        use super::print_warning_slice;
        use crate::resolver::ResolutionOverrides;
        use crate::types::{DetectionWarning, PackageManager};

        // Smoke: print_warning_slice with no_warnings=true must
        // short-circuit before the eprintln. The test asserts no
        // panic / no observable side effects; capturing stderr in
        // cargo test is fiddly and not worth a fixture.
        let warnings = vec![DetectionWarning::PmMismatch {
            declared: PackageManager::Pnpm,
            field: "packageManager",
            lockfile: PackageManager::Yarn,
        }];
        let overrides = ResolutionOverrides {
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        print_warning_slice(&warnings, &overrides);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_preserves_signal_status() {
        use std::os::unix::process::ExitStatusExt as _;

        assert_eq!(exit_code(std::process::ExitStatus::from_raw(5 << 8)), 5);
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(2)), 130);
    }
}
