//! Pipenv, Python dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `Pipfile` or `Pipfile.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("Pipfile").exists() || dir.join("Pipfile.lock").exists()
}

/// `pipenv install` or `pipenv sync` (frozen).
///
/// Plain `pipenv install` may update `Pipfile.lock` if `Pipfile`
/// has changed, which defeats the point of a frozen install in CI.
/// `pipenv sync` installs the exact versions recorded in
/// `Pipfile.lock` without touching the lockfile and errors if the
/// lockfile is missing. That's the documented "deterministic
/// install" command for Pipenv (similar to `pipenv install
/// --deploy` but stricter, `--deploy` keeps the install verb,
/// `sync` is the canonical name).
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = super::program::command("pipenv");
    if frozen {
        c.arg("sync");
    } else {
        c.arg("install");
    }
    c
}

/// `pipenv run <script> [args...]`, run a `[project.scripts]` console
/// entry point inside the project's virtualenv.
pub(crate) fn run_cmd(script: &str, args: &[String]) -> Command {
    let mut c = super::program::command("pipenv");
    c.arg("run").arg(script).args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::{install_cmd, run_cmd};

    #[test]
    fn install_unfrozen_uses_install_subcommand() {
        let args: Vec<_> = install_cmd(false)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["install"]);
    }

    #[test]
    fn install_frozen_uses_sync_subcommand() {
        let args: Vec<_> = install_cmd(true)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["sync"]);
    }

    #[test]
    fn run_cmd_forwards_script_and_args() {
        let args: Vec<_> = run_cmd("serve", &["--port".into(), "8000".into()])
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["run", "serve", "--port", "8000"]);
    }
}
