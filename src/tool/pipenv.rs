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
/// --deploy` but stricter: `--deploy` keeps the install verb,
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
pub(crate) fn run_cmd(script: &str, args: &[String], _verbosity: super::HostVerbosity) -> Command {
    // Both verbosity axes no-op here. pipenv's `--quiet`/`PIPENV_QUIET` only
    // hushes its own "Loading .env…" line, which pipenv already writes to
    // stderr — so it was never the stdout-contamination this feature targets,
    // and there's no stdout-diversion primitive to honor the stream axis either.
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
        let args: Vec<_> = run_cmd(
            "serve",
            &["--port".into(), "8000".into()],
            crate::tool::HostVerbosity::default(),
        )
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
        assert_eq!(args, ["run", "serve", "--port", "8000"]);
    }
}
