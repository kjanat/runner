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

#[cfg(test)]
mod tests {
    use super::exec_cmd;

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
}
