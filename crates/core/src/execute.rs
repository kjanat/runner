//! Spawn a plan. Adds nothing and decides nothing.

use std::io;
use std::process::{Child, Command, ExitStatus};

use crate::plan::{Plan, Trust};

/// The command `plan` describes.
///
/// # Errors
/// Returns an error when the planned executable path cannot be represented.
///
/// # Panics
///
/// In debug builds, when the plan carries no evidence.
pub fn command(plan: &Plan) -> io::Result<Command> {
    debug_assert!(!plan.because.is_empty(), "a plan without evidence is a bug");
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
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot construct the planned PATH: {error}"),
            )
        })?;
        cmd.env("PATH", joined);
    }
    Ok(cmd)
}

/// Spawn `plan` and wait for it.
///
/// # Errors
///
/// When the program cannot be spawned or waited on.
///
/// # Panics
///
/// In debug builds, when the plan carries no evidence.
pub fn execute(plan: &Plan) -> io::Result<ExitStatus> {
    status(plan, &mut command(plan)?)
}

/// Execute a configured plan with caller-selected stdio.
///
/// # Errors
/// Returns process spawn or wait errors.
///
/// # Panics
/// In debug builds, refuses missing evidence and changed argv.
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

    use super::command;
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
            warnings: Vec::new(),
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
            node: None,
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a plan without evidence")]
    fn command_construction_rejects_missing_evidence() {
        let mut made = plan(&["runner-must-not-spawn"], Trust::Project);
        made.because.clear();
        let _ = command(&made).unwrap();
    }

    #[test]
    fn explicit_shell_arguments_are_preserved() {
        let made = plan(&["bash", "-lc", "printf hello"], Trust::Host);
        let command = command(&made).unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-lc", "printf hello"]
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "planned argv")]
    fn configured_execution_rejects_changed_argv() {
        let made = plan(&["runner-must-not-spawn"], Trust::Project);
        let mut cmd = command(&made).unwrap();
        cmd.arg("changed");
        let _ = super::spawn(&made, &mut cmd);
    }

    #[test]
    fn host_trust_never_prepends_project_bin_dirs() {
        let host = command(&plan(&["mise", "install"], Trust::Host)).unwrap();
        assert!(
            host.get_envs()
                .all(|(key, _)| key != std::ffi::OsStr::new("PATH"))
        );
        let project = command(&plan(&["npm", "test"], Trust::Project)).unwrap();
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
