//! uv — fast Python package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `uv.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("uv.lock").exists()
}

/// `uv sync [--frozen]`
pub(crate) fn install_cmd(frozen: bool) -> Command {
    let mut c = super::program::command("uv");
    c.arg("sync");
    if frozen {
        c.arg("--frozen");
    }
    c
}

/// `uv run <script> [args...]` — run a `[project.scripts]` console
/// entry point inside the project environment.
///
/// `uv run` resolves the name against the scripts installed for the
/// project (the PEP 621 `[project.scripts]` entry points), syncing the
/// environment first if needed — exactly the dispatch path a
/// `[project.scripts]` task wants. This is distinct from [`exec_cmd`]
/// (`uvx`), which fetches and runs an arbitrary tool from `PyPI`.
pub(crate) fn run_cmd(script: &str, args: &[String]) -> Command {
    let mut c = super::program::command("uv");
    c.arg("run").arg(script).args(args);
    c
}

/// `uvx <args...>` — uv's `npx`-equivalent (i.e. `uv tool run`).
///
/// Runs a tool from `PyPI` in an ephemeral environment without
/// installing it permanently into the project venv. This is the
/// right primitive for the arbitrary-command exec fallback —
/// `uv run` is for the project's own Python scripts /
/// `pyproject.toml#project.scripts.<name>` entries, not for
/// `npx`-style "fetch and run any binary."
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("uvx");
    c.args(args);
    c
}

/// `uv run <file> [args...]` — execute a local Python script inside the
/// project environment. `uv run` accepts a script path directly, syncing
/// the environment first when needed. Distinct from [`exec_cmd`] (`uvx`),
/// which fetches and runs a `PyPI` tool.
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command("uv");
    c.arg("run").arg(file).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{exec_cmd, run_cmd, run_file_cmd};

    #[test]
    fn run_uses_uv_run_with_script_and_args() {
        // `runner run greenpy --flag` on a uv project should build
        // `uv run greenpy --flag` — the project-environment dispatch
        // for a `[project.scripts]` entry point, not the `uvx`
        // fetch-and-run path.
        let built: Vec<_> = run_cmd("greenpy", &[String::from("--flag")])
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "greenpy", "--flag"]);
    }

    #[test]
    fn exec_uses_uvx_passthrough() {
        // `runner --pm uv run ruff check` should build
        // `uvx ruff check` — uvx is the `uv tool run` shorthand and
        // is the npx-equivalent. `uv run` (the previous
        // implementation) only finds binaries already installed in
        // the project venv, which is a different code path.
        let args = [String::from("ruff"), String::from("check")];
        let built: Vec<_> = exec_cmd(&args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["ruff", "check"]);
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
