//! Spawn a plan. Adds nothing and decides nothing.

use std::io;
use std::process::{Child, Command, ExitStatus};

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
/// # Panics
///
/// In debug builds, when the plan carries no evidence or invokes a shell.
pub fn command(plan: &Plan) -> Command {
    debug_assert!(!plan.because.is_empty(), "a plan without evidence is a bug");
    debug_assert!(
        !invokes_a_shell(plan),
        "a plan is an argv, never a shell string"
    );
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
    status(plan, &mut command(plan))
}

/// Execute a configured plan with caller-selected stdio.
///
/// # Errors
/// Returns process spawn or wait errors.
///
/// # Panics
/// In debug builds, refuses missing evidence, shell strings, and changed argv.
pub fn status(plan: &Plan, command: &mut Command) -> io::Result<ExitStatus> {
    validate_command(plan, command);
    command.status()
}

/// Spawn a configured plan for a parallel caller.
///
/// # Errors
/// Returns process spawn errors.
///
/// # Panics
/// The same invariant checks as [`status`].
pub fn spawn(plan: &Plan, command: &mut Command) -> io::Result<Child> {
    validate_command(plan, command);
    command.spawn()
}

fn validate_command(plan: &Plan, command: &Command) {
    debug_assert!(!plan.because.is_empty(), "a plan without evidence is a bug");
    debug_assert!(
        !invokes_a_shell(plan),
        "a plan is an argv, never a shell string"
    );
    debug_assert!(
        plan.argv
            .iter()
            .map(std::ffi::OsString::as_os_str)
            .eq(std::iter::once(command.get_program()).chain(command.get_args())),
        "execution must use the planned argv"
    );
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
            because: vec![crate::Evidence {
                provider: Some(ProviderId::Npm),
                signal: None,
                at: PathBuf::from("/project/package.json"),
                scope: Scope::Root,
                weight: crate::Weight::Declared,
                declared: None,
            }],
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
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a plan without evidence")]
    fn command_construction_rejects_missing_evidence() {
        let mut made = plan(&["runner-must-not-spawn"], Trust::Project);
        made.because.clear();
        let _ = command(&made);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "never a shell string")]
    fn command_construction_rejects_shell_strings() {
        let _ = command(&plan(&["sh", "-c", "exit 0"], Trust::Project));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "planned argv")]
    fn configured_execution_rejects_changed_argv() {
        let made = plan(&["runner-must-not-spawn"], Trust::Project);
        let mut cmd = command(&made);
        cmd.arg("changed");
        let _ = super::spawn(&made, &mut cmd);
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
