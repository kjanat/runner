//! Subcommand implementations: info, run, install, clean, list, completions.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use colored::Colorize;

use crate::types::ProjectContext;

mod clean;
mod completions;
mod info;
mod install;
mod list;
mod run;

pub(crate) use clean::clean;
pub(crate) use completions::completions;
pub(crate) use info::info;
pub(crate) use install::install;
pub(crate) use list::list;
pub(crate) use run::run;

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

fn print_warnings(ctx: &ProjectContext) {
    for warning in &ctx.warnings {
        eprintln!(
            "{} {}: {}",
            "warn:".yellow().bold(),
            warning.source,
            warning.detail,
        );
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

    #[cfg(unix)]
    #[test]
    fn exit_code_preserves_signal_status() {
        use std::os::unix::process::ExitStatusExt as _;

        assert_eq!(exit_code(std::process::ExitStatus::from_raw(5 << 8)), 5);
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(2)), 130);
    }
}
