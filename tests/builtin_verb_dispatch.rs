//! Integration coverage for the builtin-verb dispatch split.
//!
//! The explicit `runner <verb>` subcommand is ALWAYS the builtin: a
//! same-named project task never shadows it. The run path (`run <verb>` /
//! `runner run <verb>`) runs a same-named task when one exists, else falls
//! back to the builtin's default (no-flag) form.
//!
//! `list` is the exercise verb here: its builtin is cheap (renders the task
//! list, spawns nothing), unlike `install`/`clean` which would shell out to
//! real package managers. The fallback helper is verb-agnostic, so proving
//! `list` exercises the same code path that serves every builtin verb.
//!
//! Fixtures use `just`; if it's not on PATH the assertions are skipped
//! rather than failing, matching `info_deprecation.rs`.

use std::path::PathBuf;
use std::process::Command;

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn run_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_run"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn just_available() -> bool {
    Command::new("just")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn runner_list_is_always_builtin_even_with_a_list_task() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // A project task named `list` does NOT shadow the explicit subcommand.
    let output = Command::new(runner_binary())
        .args(["--dir", fixture("list-shadowed").to_str().unwrap(), "list"])
        .output()
        .expect("runner binary spawns");

    assert!(output.status.success(), "`runner list` should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("project-list-recipe-ran"),
        "the project `list` recipe must NOT run for the explicit subcommand. stdout: {stdout}",
    );
    // Builtin list shape: the recipe names appear as listed tasks.
    assert!(
        stdout.contains("list") && stdout.contains("build"),
        "expected the justfile recipes in the builtin list output. stdout: {stdout}",
    );
}

#[test]
fn run_list_runs_a_same_named_task() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let dir = fixture("list-shadowed");
    let dir = dir.to_str().unwrap();
    let invocations: [(&str, PathBuf, Vec<&str>); 2] = [
        ("run list", run_binary(), vec!["--dir", dir, "list"]),
        (
            "runner run list",
            runner_binary(),
            vec!["--dir", dir, "run", "list"],
        ),
    ];
    for (label, binary, args) in invocations {
        let output = Command::new(binary)
            .args(&args)
            .output()
            .expect("binary spawns");

        assert!(output.status.success(), "`{label}` should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("project-list-recipe-ran"),
            "`{label}` should run the project `list` recipe. stdout: {stdout}",
        );
    }
}

#[test]
fn run_list_falls_back_to_builtin_when_no_task() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // `info-deprecated` defines `build`/`test` but no `list` task, so the
    // run path falls back to the builtin list (rather than the PM-exec
    // path, which would try to spawn a bare `list` binary and fail).
    let dir = fixture("info-deprecated");
    let dir = dir.to_str().unwrap();
    for (label, binary, args) in [
        ("run list", run_binary(), vec!["--dir", dir, "list"]),
        (
            "runner run list",
            runner_binary(),
            vec!["--dir", dir, "run", "list"],
        ),
    ] {
        let output = Command::new(binary)
            .args(&args)
            .output()
            .expect("binary spawns");

        assert!(
            output.status.success(),
            "`{label}` should fall back to the builtin list and exit 0. stderr: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("build") && stdout.contains("test"),
            "`{label}` builtin-list fallback should render the justfile tasks. stdout: {stdout}",
        );
    }
}
