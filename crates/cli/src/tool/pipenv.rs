//! Pipenv, Python dependency manager.

use std::path::Path;
#[cfg(test)]
use std::process::Command;

/// Detected via `Pipfile` or `Pipfile.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("Pipfile").exists() || dir.join("Pipfile.lock").exists()
}

/// `pipenv run <script> [args...]`, run a `[project.scripts]` console
/// entry point inside the project's virtualenv.
#[cfg(test)]
pub(crate) fn run_cmd(script: &str, args: &[String], verbosity: super::HostVerbosity) -> Command {
    let mut c = super::program::command("pipenv");
    if verbosity.silences() {
        c.arg("--quiet");
    }
    c.arg("run").arg(script).args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::run_cmd;

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
