//! `runner install` in a project with no package manager runs the project's
//! own `install` task instead of failing. Fixtures use `just`; if it is not
//! on PATH the assertion is skipped rather than failing.

use std::path::PathBuf;
use std::process::Command;

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn fixture_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("runner-install-task-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::copy(
        fixture("install-task").join("justfile"),
        dir.join("justfile"),
    )
    .expect("fixture copies");
    dir
}

fn just_available() -> bool {
    Command::new("just")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn install_runs_the_install_task_when_no_package_manager_exists() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Copied out of the repository so detection cannot walk up to this
    // repository's own manifests and lockfiles.
    let dir = fixture_dir("runs");
    let output = Command::new(runner_binary())
        .args(["--dir", dir.to_str().unwrap(), "install"])
        .output()
        .expect("runner binary spawns");
    let _ = std::fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "`runner install` should exit 0. stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains("task-install-ran"),
        "the `install` task must run. stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        !stdout.contains("build-ran"),
        "only the `install` task runs. stdout: {stdout}"
    );
}

#[test]
fn install_refuses_an_unobserved_explicit_package_manager() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let dir = fixture_dir("override");
    let output = Command::new(runner_binary())
        .args(["--dir", dir.to_str().unwrap(), "install"])
        .env("RUNNER_PM", "pnpm")
        .env("PATH", dir.join("empty-path"))
        .output()
        .expect("runner binary spawns");
    let _ = std::fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an unobserved explicit package manager must fail. stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.contains("pnpm"),
        "the error names the listed manager. stderr: {stderr}"
    );
    assert!(
        !stdout.contains("task-install-ran"),
        "the install task must not run. stdout: {stdout}"
    );
}
