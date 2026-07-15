//! Integration coverage for the deprecated `info` subcommand.
//!
//! `runner info` is a hidden, deprecated alias for `runner list`: it
//! warns on stderr then renders the task list. The explicit subcommand is
//! ALWAYS the builtin, a project task named `info` no longer shadows it;
//! the task is reachable via `run info` / `runner run info`. Bare `runner`
//! (no subcommand) keeps the project dashboard and is unaffected.
//!
//! Fixtures use `just`; if it's not on PATH the just-dependent
//! assertions are skipped rather than failing, matching
//! `chain_integration.rs`.

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
fn info_warns_and_renders_list_when_not_shadowed() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("info-deprecated").to_str().unwrap(),
            "info",
        ])
        .output()
        .expect("runner binary spawns");

    assert!(output.status.success(), "`runner info` should exit 0");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deprecated"),
        "expected deprecation warning on stderr, got: {stderr}",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // List shape, NOT the info dashboard: no `Package Managers` banner,
    // the justfile recipes are present.
    assert!(
        !stdout.contains("Package Managers"),
        "info-as-list must not print the dashboard banner. stdout: {stdout}",
    );
    assert!(
        stdout.contains("build") && stdout.contains("test"),
        "expected the justfile tasks in list output. stdout: {stdout}",
    );
}

#[test]
fn info_emits_github_actions_annotation_under_ci() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("info-deprecated").to_str().unwrap(),
            "info",
        ])
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("runner binary spawns");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("::warning title=Deprecation::"),
        "expected a GitHub Actions warning annotation on stderr under CI, got: {stderr}",
    );
    // The annotation must not leak into stdout (keeps `--json` pipes
    // clean).
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("::warning"),
        "GHA annotation must stay on stderr, not stdout. stdout: {stdout}",
    );
}

#[test]
fn info_omits_github_annotation_outside_ci() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Explicitly clear the var so the test is deterministic even when
    // the test host itself runs under GitHub Actions.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("info-deprecated").to_str().unwrap(),
            "info",
        ])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deprecated"),
        "plain deprecation warning still expected, got: {stderr}",
    );
    assert!(
        !stderr.contains("::warning"),
        "no GHA annotation outside CI, got: {stderr}",
    );
}

#[test]
fn info_json_maps_to_list_json_with_tasks_array() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("info-deprecated").to_str().unwrap(),
            "info",
            "--json",
        ])
        .output()
        .expect("runner binary spawns");

    assert!(
        output.status.success(),
        "`runner info --json` should exit 0"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // `serde_json` is a normal dependency, not a dev-dependency, so it
    // isn't linkable from this integration crate, assert on the
    // serialized text instead. list's JSON view emits a `"tasks"` array
    // containing the justfile recipes; info's task-less view would emit
    // neither. A `"tasks"` key plus a recipe name proves the alias
    // mapped to `list --json`.
    assert!(
        stdout.contains("\"tasks\""),
        "expected a `tasks` key (list json view), got: {stdout}",
    );
    assert!(
        stdout.contains("\"build\"") && stdout.contains("\"test\""),
        "expected the justfile recipes in the json tasks array, got: {stdout}",
    );
}

#[test]
fn runner_info_is_the_deprecated_alias_even_when_a_task_is_named_info() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // The explicit subcommand is always the builtin: a project task named
    // `info` does NOT shadow it. `runner info` stays the deprecated
    // alias-for-list even when the fixture defines an `info` recipe.
    let output = Command::new(runner_binary())
        .args(["--dir", fixture("info-shadowed").to_str().unwrap(), "info"])
        .output()
        .expect("runner binary spawns");

    assert!(output.status.success(), "`runner info` should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("project-info-recipe-ran"),
        "the project `info` recipe must NOT run for the explicit subcommand. stdout: {stdout}",
    );
    assert!(
        stderr.contains("deprecated"),
        "deprecation warning expected, the task no longer shadows. stderr: {stderr}",
    );
    // List shape: the recipe names are present, the dashboard banner is not.
    assert!(
        stdout.contains("info") && stdout.contains("build"),
        "expected the justfile recipes in list output. stdout: {stdout}",
    );
    assert!(
        !stdout.contains("Package Managers"),
        "info-as-list must not print the dashboard banner. stdout: {stdout}",
    );
}

#[test]
fn run_info_runs_a_same_named_task() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // The run path is where a same-named task is reachable: `run info`
    // (and `runner run info`) run the project `info` recipe, no deprecation.
    let dir = fixture("info-shadowed");
    let dir = dir.to_str().unwrap();
    let invocations: [(&str, PathBuf, Vec<&str>); 2] = [
        ("run info", run_binary(), vec!["--dir", dir, "info"]),
        (
            "runner run info",
            runner_binary(),
            vec!["--dir", dir, "run", "info"],
        ),
    ];
    for (label, binary, args) in invocations {
        let output = Command::new(binary)
            .args(&args)
            .output()
            .expect("binary spawns");

        assert!(output.status.success(), "`{label}` should exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("project-info-recipe-ran"),
            "`{label}` should run the project `info` recipe. stdout: {stdout}",
        );
        assert!(
            !stderr.contains("deprecated"),
            "`{label}` runs the task, not the deprecated alias. stderr: {stderr}",
        );
    }
}

#[test]
fn bare_runner_still_shows_the_dashboard() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // No subcommand → the project dashboard survives the `info`
    // subcommand deprecation.
    let output = Command::new(runner_binary())
        .args(["--dir", fixture("info-deprecated").to_str().unwrap()])
        .output()
        .expect("runner binary spawns");

    assert!(output.status.success(), "bare `runner` should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("Task Runners"),
        "bare `runner` must still print the dashboard banner. stdout: {stdout}",
    );
    assert!(
        !stderr.contains("deprecated"),
        "bare `runner` must not emit the info deprecation warning. stderr: {stderr}",
    );
}
