//! Regression coverage for actionable process-spawn errors.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static PROJECT_ID: AtomicU32 = AtomicU32::new(0);

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let id = PROJECT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "runner-spawn-diagnostic-{tag}-{}-{id}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp project");
        Self { path }
    }

    fn file(self, name: &str, contents: &str) -> Self {
        std::fs::write(self.path.join(name), contents).expect("write project file");
        self
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn run_in(project: &TempProject, args: &[&str]) -> Output {
    let empty_path = project.path().join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("create isolated PATH");

    let mut command = Command::new(runner_binary());
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RUNNER_")
        {
            command.env_remove(&key);
        }
    }
    command
        .env("PATH", empty_path)
        .arg("--dir")
        .arg(project.path())
        .args(args)
        .output()
        .expect("runner should execute")
}

fn bun_project(tag: &str) -> TempProject {
    TempProject::new(tag).file(
        "package.json",
        r#"{
  "packageManager": "bun@1.3.14",
  "scripts": {
    "build": "true",
    "test": "true"
  }
}
"#,
    )
}

fn uv_project(tag: &str) -> TempProject {
    TempProject::new(tag)
        .file(
            "pyproject.toml",
            r#"[project]
name = "spawn-diagnostic"
version = "0.0.0"

[project.scripts]
hello = "spawn_diagnostic:main"
"#,
        )
        .file("uv.lock", "")
}

fn assert_manifest_bun_diagnostic(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(
            "bun via package.json \"packageManager\" was selected, but its executable was not \
             found on PATH",
        ),
        "missing actionable package-manager diagnostic. stderr: {stderr}",
    );
}

#[test]
fn manifest_selected_missing_pm_reports_provenance() {
    let project = bun_project("serial");
    let output = run_in(&project, &["run", "build"]);

    assert_manifest_bun_diagnostic(&output);
}

#[test]
fn quiet_manifest_selected_missing_pm_reports_provenance() {
    let project = bun_project("quiet");
    let output = run_in(&project, &["-q", "run", "build"]);

    assert_manifest_bun_diagnostic(&output);
}

#[test]
fn parallel_manifest_selected_missing_pm_reports_provenance() {
    let project = bun_project("parallel");
    let output = run_in(&project, &["run", "-p", "build", "test"]);

    assert_manifest_bun_diagnostic(&output);
}

#[test]
fn pyproject_script_missing_pm_reports_provenance() {
    let project = uv_project("python");
    let output = run_in(&project, &["run", "hello"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(
            "uv via detected Python project was selected, but its executable was not found on PATH",
        ),
        "missing actionable Python package-manager diagnostic. stderr: {stderr}",
    );
}

#[test]
fn missing_direct_command_keeps_generic_spawn_error() {
    let project = TempProject::new("direct");
    let output = run_in(&project, &["run", "definitely-not-a-binary-xyz"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(!stderr.contains("was selected"), "stderr: {stderr}");
    assert!(!stderr.contains("packageManager"), "stderr: {stderr}");
}
