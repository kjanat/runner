//! uv, fast Python package manager.

use std::path::Path;
#[cfg(test)]
use std::process::Command;

/// Detected via `uv.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("uv.lock").exists()
}

/// `uv run <script> [args...]`, run a `[project.scripts]` console
/// entry point inside the project environment.
///
/// `uv run` resolves the name against the scripts installed for the
/// project (the PEP 621 `[project.scripts]` entry points), syncing the
/// environment first if needed, exactly the dispatch path a
/// `[project.scripts]` task wants. This is distinct from [`exec_cmd`]
/// (`uvx`), which fetches and runs an arbitrary tool from `PyPI`.
#[cfg(test)]
pub(crate) fn run_cmd(script: &str, args: &[String], verbosity: super::HostVerbosity) -> Command {
    let mut c = super::program::command("uv");
    // uv's global `-q`/`--quiet` precedes the `run` subcommand. It has no
    // stdout-diversion primitive, so the stream axis no-ops.
    if verbosity.silences() {
        c.arg("--quiet");
    }
    c.arg("run").arg(script).args(args);
    c
}

/// `uv run <file> [args...]`, execute a local Python script inside the
/// project environment. `uv run` accepts a script path directly, syncing
/// the environment first when needed. Distinct from [`exec_cmd`] (`uvx`),
/// which fetches and runs a `PyPI` tool.
#[cfg(test)]
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command("uv");
    c.arg("run").arg(file).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{run_cmd, run_file_cmd};

    #[test]
    fn run_uses_uv_run_with_script_and_args() {
        // `runner run greenpy --flag` on a uv project should build
        // `uv run greenpy --flag`, the project-environment dispatch
        // for a `[project.scripts]` entry point, not the `uvx`
        // fetch-and-run path.
        let built: Vec<_> = run_cmd(
            "greenpy",
            &[String::from("--flag")],
            crate::tool::HostVerbosity::default(),
        )
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

        assert_eq!(built, ["run", "greenpy", "--flag"]);
    }

    #[test]
    fn run_file_cmd_uses_uv_run_with_path() {
        // A local `.py` file in a uv project dispatches as
        // `uv run <file>` (project-environment execution), not `uvx`.
        let cmd = run_file_cmd(Path::new("/abs/task.py"), &[String::from("--once")]);
        let built: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), "uv");
        assert_eq!(built, ["run", "/abs/task.py", "--once"]);
    }
}
