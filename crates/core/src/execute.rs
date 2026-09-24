//! Spawn a plan. Adds nothing and decides nothing.

use std::io;
use std::process::{Command, ExitStatus};

use crate::plan::{Plan, Trust};

/// Whether `plan` hands a command line to a shell to parse.
#[must_use]
pub fn invokes_a_shell(plan: &Plan) -> bool {
    let words: Vec<String> = plan
        .argv
        .iter()
        .map(|word| word.to_string_lossy().to_ascii_lowercase())
        .collect();
    let program = words
        .first()
        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_owned())
        .unwrap_or_default();
    let flag = words.get(1).map(String::as_str);
    match program.trim_end_matches(".exe") {
        "sh" | "bash" | "zsh" | "dash" => flag == Some("-c"),
        "cmd" => flag == Some("/c"),
        "powershell" | "pwsh" => words.iter().any(|w| w == "-command" || w == "-c"),
        _ => false,
    }
}

/// The command `plan` describes.
#[must_use]
pub fn command(plan: &Plan) -> Command {
    let mut argv = plan.argv.iter();
    let program = argv.next().cloned().unwrap_or_default();
    let mut cmd = Command::new(program);
    cmd.args(argv)
        .current_dir(&plan.cwd)
        .envs(plan.env.iter().cloned());
    for key in &plan.env_remove {
        cmd.env_remove(key);
    }
    if plan.trust == Trust::Project && !plan.path_prepend.is_empty() {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(
            plan.path_prepend
                .iter()
                .cloned()
                .chain(std::env::split_paths(&inherited)),
        )
        .unwrap_or(inherited);
        cmd.env("PATH", joined);
    }
    cmd
}

/// Spawn `plan` and wait for it.
///
/// # Errors
///
/// When the program cannot be spawned or waited on.
///
/// # Panics
///
/// In debug builds, when the plan carries no evidence or invokes a shell.
pub fn execute(plan: &Plan) -> io::Result<ExitStatus> {
    debug_assert!(!plan.because.is_empty(), "a plan without evidence is a bug");
    debug_assert!(
        !invokes_a_shell(plan),
        "a plan is an argv, never a shell string"
    );
    command(plan).status()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{command, invokes_a_shell};
    use crate::plan::{Plan, Trust};
    use crate::provider::ProviderId;
    use crate::reach::Reach;
    use crate::scope::Scope;

    fn plan(argv: &[&str], trust: Trust) -> Plan {
        Plan {
            provider: Some(ProviderId::Npm),
            found: None,
            argv: argv.iter().map(OsString::from).collect(),
            cwd: std::env::temp_dir(),
            env: vec![(OsString::from("RUNNER_TEST"), OsString::from("1"))],
            env_remove: Vec::new(),
            path_prepend: vec![PathBuf::from("/project/node_modules/.bin")],
            trust,
            reach: Reach::Local,
            clamps: Vec::new(),
            because: Vec::new(),
            decided_by: Vec::new(),
            scope: Scope::Root,
        }
    }

    #[test]
    fn shell_strings_are_recognised() {
        assert!(invokes_a_shell(&plan(
            &["sh", "-c", "npm test"],
            Trust::Project
        )));
        assert!(invokes_a_shell(&plan(
            &["cmd", "/c", "npm test"],
            Trust::Project
        )));
        assert!(invokes_a_shell(&plan(
            &["powershell", "-Command", "npm test"],
            Trust::Project
        )));
        assert!(!invokes_a_shell(&plan(&["npm", "test"], Trust::Project)));
        assert!(!invokes_a_shell(&plan(
            &["sh", "script.sh"],
            Trust::Project
        )));
    }

    #[test]
    fn host_trust_never_prepends_project_bin_dirs() {
        let host = command(&plan(&["mise", "install"], Trust::Host));
        assert!(
            host.get_envs()
                .all(|(key, _)| key != std::ffi::OsStr::new("PATH"))
        );
        let project = command(&plan(&["npm", "test"], Trust::Project));
        let path = project
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("project trust sets PATH");
        assert!(
            std::env::split_paths(path)
                .next()
                .is_some_and(|dir| dir == std::path::Path::new("/project/node_modules/.bin"))
        );
    }
}
