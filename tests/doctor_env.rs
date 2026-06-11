//! Integration coverage for doctor's lenient handling of invalid
//! env-sourced overrides.
//!
//! `runner doctor` exists to diagnose a misconfigured environment, so a
//! garbage `RUNNER_PM` (e.g. PowerShell's unquoted `$env:RUNNER_PM=deno`
//! capturing deno's REPL banner) must degrade to a warning on the
//! report instead of killing the command. Every other command — and an
//! explicit `--pm` flag, even on doctor — stays strict.
//!
//! Env vars are injected per spawned child (never `std::env::set_var`),
//! so these tests are safe under the parallel test runner.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

/// Minimal self-cleaning temp project: a directory holding only a
/// `Cargo.toml`, so detection finds exactly one PM (cargo) and the
/// doctor report is deterministic.
struct TempProject {
    path: PathBuf,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

impl TempProject {
    fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "runner-doctor-env-{prefix}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp project dir");
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        )
        .expect("write Cargo.toml");
        Self { path }
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

/// A value shaped like the PowerShell footgun output: multi-line, with
/// an ANSI escape.
const CAPTURED_BANNER: &str = "Deno 2.8.2 exit using ctrl+d\n\u{1b}[33mREPL is running\u{1b}[0m";

#[test]
fn doctor_survives_env_pm_garbage_and_reports_it() {
    let project = TempProject::new("doctor-lenient");
    let output = Command::new(runner_binary())
        .args(["--dir", project.path().to_str().unwrap(), "doctor"])
        .env("RUNNER_PM", CAPTURED_BANNER)
        .env_remove("RUNNER_RUNNER")
        .output()
        .expect("runner binary spawns");

    assert!(
        output.status.success(),
        "doctor must survive env garbage. stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("RUNNER_PM"),
        "the report must attribute the invalid override. output: {combined}",
    );
    assert!(
        combined.contains("ignored"),
        "the report must say the value was ignored. output: {combined}",
    );
}

#[test]
fn other_commands_stay_strict_on_env_pm_garbage() {
    let project = TempProject::new("list-strict");
    let output = Command::new(runner_binary())
        .args(["--dir", project.path().to_str().unwrap(), "list"])
        .env("RUNNER_PM", CAPTURED_BANNER)
        .env_remove("RUNNER_RUNNER")
        .output()
        .expect("runner binary spawns");

    assert!(
        !output.status.success(),
        "non-doctor commands must keep failing on env garbage",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("RUNNER_PM"),
        "the error must name the env source. stderr: {stderr}",
    );
}

#[test]
fn doctor_with_cli_pm_garbage_still_errors() {
    let project = TempProject::new("doctor-cli-strict");
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            project.path().to_str().unwrap(),
            "--pm",
            "zoot",
            "doctor",
        ])
        .env_remove("RUNNER_PM")
        .env_remove("RUNNER_RUNNER")
        .output()
        .expect("runner binary spawns");

    assert!(
        !output.status.success(),
        "an explicit bad --pm flag must stay fatal, even on doctor",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown package manager"),
        "stderr: {stderr}",
    );
}
