//! Subcommand implementations: info, run, install, clean, list, exec, completions.

use std::path::Path;
use std::process::{Command, Stdio};

mod clean;
mod completions;
mod exec;
mod info;
mod install;
mod list;
mod run;

pub(crate) use clean::clean;
pub(crate) use completions::completions;
pub(crate) use exec::exec;
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::configure_command;

    #[test]
    fn configure_command_sets_current_dir() {
        let dir = Path::new("/tmp");
        let mut command = Command::new("runner-test-command");

        configure_command(&mut command, dir);

        assert_eq!(command.get_current_dir(), Some(dir));
    }
}
