//! Regression coverage for actionable process-spawn errors.

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;
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
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create project file parent");
        }
        std::fs::write(path, contents).expect("write project file");
        self
    }

    #[cfg(unix)]
    fn executable(self, name: &str, contents: &str) -> Self {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create executable parent");
        }
        write_executable(&path, contents);
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

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::io::Write as _;
    let mut child = std::process::Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("sh writes the executable");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(contents.as_bytes())
        .expect("contents written");
    assert!(child.wait().expect("sh exits").success());
}

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn run_in(project: &TempProject, args: &[&str]) -> Output {
    let empty_path = project.path().join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("create isolated PATH");

    let mut command = support::command(runner_binary());
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

#[test]
fn unknown_names_offer_hints_without_dispatching() {
    let project = bun_project("typos");
    for (name, hint) in [
        ("biuld", "build"),
        ("instlal", "install"),
        ("version", "--version"),
        ("doctro", "doctor"),
    ] {
        let output = run_in(&project, &[name]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{name}");
        assert!(
            stderr.contains("did you mean") && stderr.contains(hint),
            "{stderr}"
        );
        assert!(!stderr.contains("→"), "must not dispatch: {stderr}");
    }
    let output = run_in(&project, &["why", "biuld"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("did you mean `build`"));
}

#[cfg(unix)]
#[test]
fn exact_host_binary_wins_over_a_task_spelling_suggestion() {
    let project = bun_project("exact-before-typo").executable(
        "empty-path/biuld",
        "#!/bin/sh\nprintf 'exact-binary-ran\\n'\n",
    );
    let output = run_in(&project, &["biuld"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("exact-binary-ran"));
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

#[cfg(unix)]
#[test]
fn project_local_pm_uses_effective_child_path_for_diagnostics() {
    let project = bun_project("local-path").executable(
        "node_modules/.bin/bun",
        "#!/definitely/missing/interpreter\n",
    );
    let output = run_in(&project, &["run", "build"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(
            "bun via package.json \"packageManager\" was selected, but failed to launch",
        ),
        "local bun should be found on the configured child PATH. stderr: {stderr}",
    );
    assert!(
        !stderr.contains("executable was not found on PATH"),
        "diagnostic must not ignore the configured child PATH. stderr: {stderr}",
    );
}

#[test]
fn pyproject_script_missing_pm_reports_the_layer_that_chose_it() {
    let project = uv_project("python");
    let output = run_in(&project, &["run", "hello"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("uv via uv.lock was selected, but its executable was not found on PATH",),
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
