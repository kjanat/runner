//! Go modules — the Go dependency system.

use std::path::Path;
use std::process::Command;

/// Directories that may be cleaned in a Go project.
pub(crate) const CLEAN_DIRS: &[&str] = &["vendor"];

/// Detected via `go.mod`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("go.mod").exists()
}

/// `go mod download`
pub(crate) fn install_cmd() -> Command {
    let mut c = super::program::command("go");
    c.arg("mod").arg("download");
    c
}

/// `go run <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("go");
    c.arg("run").args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::exec_cmd;

    #[test]
    fn exec_uses_go_run_passthrough() {
        let args = [
            String::from("github.com/foo/tool@latest"),
            String::from("--help"),
        ];
        let built: Vec<_> = exec_cmd(&args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "github.com/foo/tool@latest", "--help"]);
    }
}
