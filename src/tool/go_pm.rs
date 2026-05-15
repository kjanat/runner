//! Go modules — the Go dependency system.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directories that may be cleaned in a Go project.
pub(crate) const CLEAN_DIRS: &[&str] = &["vendor"];

/// Detected via `go.mod`.
pub(crate) fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

pub(crate) fn find_file(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("go.mod");
    path.is_file().then_some(path)
}

/// Extract local Go commands from `cmd/<name>` packages.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    let cmd_dir = dir.join("cmd");
    if !cmd_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut tasks = Vec::new();
    for entry in fs::read_dir(cmd_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !contains_main_package(&path)? {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        tasks.push(name.to_string());
    }
    tasks.sort_unstable();
    Ok(tasks)
}

fn contains_main_package(dir: &Path) -> anyhow::Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("go") {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("_test.go"))
        {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        if content.lines().any(is_main_package_line) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_main_package_line(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("package") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(tail) = rest.strip_prefix("main") else {
        return false;
    };
    tail.is_empty()
        || tail.starts_with(char::is_whitespace)
        || tail.starts_with("//")
        || tail.starts_with("/*")
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

/// `go run ./cmd/<task> <args...>`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("go");
    c.arg("run").arg(format!("./cmd/{task}")).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{exec_cmd, extract_tasks, run_cmd};
    use crate::tool::test_support::TempDir;

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

    #[test]
    fn extract_tasks_finds_cmd_main_packages() {
        let dir = TempDir::new("go-cmd-tasks");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::create_dir_all(dir.path().join("cmd").join("serve"))
            .expect("serve dir should be created");
        fs::create_dir_all(dir.path().join("cmd").join("internal-lib"))
            .expect("internal-lib dir should be created");
        fs::write(
            dir.path().join("cmd").join("serve").join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("serve main should be written");
        fs::write(
            dir.path().join("cmd").join("internal-lib").join("lib.go"),
            "package lib\n",
        )
        .expect("lib source should be written");

        let tasks = extract_tasks(dir.path()).expect("go cmd tasks should parse");

        assert_eq!(tasks, ["serve"]);
    }

    #[test]
    fn extract_tasks_ignores_main_test_packages() {
        let dir = TempDir::new("go-cmd-test-only");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::create_dir_all(dir.path().join("cmd").join("serve"))
            .expect("serve dir should be created");
        fs::write(
            dir.path().join("cmd").join("serve").join("main_test.go"),
            "package main\n\nfunc TestServe() {}\n",
        )
        .expect("test main should be written");

        let tasks = extract_tasks(dir.path()).expect("go cmd tasks should parse");

        assert!(tasks.is_empty());
    }

    #[test]
    fn extract_tasks_accepts_commented_main_package_clause() {
        let dir = TempDir::new("go-cmd-main-comment");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::create_dir_all(dir.path().join("cmd").join("serve"))
            .expect("serve dir should be created");
        fs::write(
            dir.path().join("cmd").join("serve").join("main.go"),
            "package main // command\n\nfunc main() {}\n",
        )
        .expect("main should be written");

        let tasks = extract_tasks(dir.path()).expect("go cmd tasks should parse");

        assert_eq!(tasks, ["serve"]);
    }

    #[test]
    fn run_cmd_uses_go_run_cmd_package() {
        let args = [String::from("--port"), String::from("3000")];
        let built: Vec<_> = run_cmd("serve", &args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "./cmd/serve", "--port", "3000"]);
    }
}
