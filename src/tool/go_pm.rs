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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedTask {
    pub name: String,
    pub run_target: String,
}

/// Extract local Go commands from root and `cmd/<name>` packages.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let mut tasks = Vec::new();

    if contains_main_package(dir)?
        && let Some(name) = module_name(dir)
            .or_else(|| dir.file_name().and_then(|n| n.to_str()).map(str::to_string))
    {
        tasks.push(ExtractedTask {
            name,
            run_target: ".".to_string(),
        });
    }

    let cmd_dir = dir.join("cmd");
    if !cmd_dir.is_dir() {
        return Ok(tasks);
    }

    for entry in fs::read_dir(cmd_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !contains_main_package(&path)? {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        tasks.push(ExtractedTask {
            name: name.to_string(),
            run_target: format!("./cmd/{name}"),
        });
    }
    tasks.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

/// Last path segment of the `module` directive in `dir/go.mod`, used as the
/// root single-binary task name so it tracks the module's identity rather
/// than the filesystem location (cloning to a differently-named directory
/// must not change the task name). A trailing `/vN` major-version suffix is
/// dropped, matching how Go names the produced binary (`example.com/foo/v2`
/// builds `foo`). `None` if `go.mod` is absent or has no parseable `module`
/// line, so the caller can fall back to the directory name.
fn module_name(dir: &Path) -> Option<String> {
    let content = fs::read_to_string(dir.join("go.mod")).ok()?;
    let path = content.lines().find_map(parse_module_line)?;
    let mut segments = path.rsplit('/');
    let last = segments.next()?;
    let name = if is_major_version(last) {
        segments.next().unwrap_or(last)
    } else {
        last
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// Extract the module path from a single `go.mod` line, or `None` if the
/// line is not a `module` directive. Requires a whitespace boundary after
/// `module` so identifiers like `modulefoo` do not match, tolerates a
/// trailing `// comment`, and strips optional surrounding quotes.
fn parse_module_line(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("module")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let tok = rest.split_whitespace().next()?.trim_matches('"');
    (!tok.is_empty()).then_some(tok)
}

/// A Go major-version path segment: `v` followed by one or more digits
/// (`v2`, `v10`). These are suffixes on the module path, not the binary
/// name, so they are skipped when deriving the task name.
fn is_major_version(seg: &str) -> bool {
    seg.strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
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

/// `go run <target> <args...>`
pub(crate) fn run_cmd(target: &str, args: &[String]) -> Command {
    let mut c = super::program::command("go");
    c.arg("run").arg(target).args(args);
    c
}

/// `go run <file> [args...]` — execute a single-file Go program by path.
/// Generalizes the slash-containing-token Go special case in the PM-exec
/// fallback to the local-file dispatch path.
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command("go");
    c.arg("run").arg(file).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ExtractedTask, exec_cmd, extract_tasks, run_cmd, run_file_cmd};
    use crate::tool::test_support::TempDir;

    #[test]
    fn run_file_cmd_uses_go_run_with_path() {
        use std::path::Path;

        let cmd = run_file_cmd(Path::new("/abs/main.go"), &[String::from("serve")]);
        let built: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), "go");
        assert_eq!(built, ["run", "/abs/main.go", "serve"]);
    }

    fn task(name: &str, run_target: &str) -> ExtractedTask {
        ExtractedTask {
            name: name.to_string(),
            run_target: run_target.to_string(),
        }
    }

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

        assert_eq!(tasks, [task("serve", "./cmd/serve")]);
    }

    #[test]
    fn extract_tasks_root_name_from_go_mod_module() {
        let dir = TempDir::new("go-root-main");
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/kjanat/some-cli-app // root\n",
        )
        .expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("root main should be written");

        // Last module-path segment, not the (temp, randomized) directory name.
        let tasks = extract_tasks(dir.path()).expect("go root task should parse");

        assert_eq!(tasks, [task("some-cli-app", ".")]);
    }

    #[test]
    fn extract_tasks_root_name_drops_major_version_suffix() {
        let dir = TempDir::new("go-root-v2");
        fs::write(dir.path().join("go.mod"), "module example.com/widget/v2\n")
            .expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("root main should be written");

        let tasks = extract_tasks(dir.path()).expect("go root task should parse");

        assert_eq!(tasks, [task("widget", ".")]);
    }

    #[test]
    fn extract_tasks_root_name_falls_back_to_dir_without_module_line() {
        let dir = TempDir::new("go-root-no-module");
        // go.mod present (project still detected) but no `module` directive.
        fs::write(dir.path().join("go.mod"), "go 1.22\n").expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("root main should be written");

        let tasks = extract_tasks(dir.path()).expect("go root task should parse");
        let name = dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temp dir should have utf-8 file name");

        assert_eq!(tasks, [task(name, ".")]);
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

        assert_eq!(tasks, [task("serve", "./cmd/serve")]);
    }

    #[test]
    fn run_cmd_uses_go_run_target() {
        let args = [String::from("--port"), String::from("3000")];
        let built: Vec<_> = run_cmd("./cmd/serve", &args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "./cmd/serve", "--port", "3000"]);
    }
}
