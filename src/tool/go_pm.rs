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

/// `go run <args...>` — Go's `npx`-equivalent for module paths.
///
/// `go run example.com/user/repo@latest` fetches the module to the
/// build cache and runs its `main` package without permanently
/// installing the binary (no write to `$GOBIN`). Bare-name targets
/// won't resolve — `go run` expects either a local file
/// (`./main.go`), a package path inside the current module (`.`),
/// or a fully-qualified module path with a version suffix. Same
/// caveat as `deno x <target>`: the runner passes the user's
/// `--pm go run` intent verbatim and lets Go's resolver report
/// the error if the argument isn't a valid module path.
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
        // `runner --pm go run example.com/foo@latest --flag value`
        // should build `go run example.com/foo@latest --flag value`.
        let args = [
            String::from("example.com/foo@latest"),
            String::from("--flag"),
            String::from("value"),
        ];
        let built: Vec<_> = exec_cmd(&args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "example.com/foo@latest", "--flag", "value"]);
    }
}
