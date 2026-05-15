//! Integration coverage for the deprecated `info` subcommand.
//!
//! `runner info` is a hidden, deprecated alias for `runner list`: it
//! warns on stderr then renders the task list. A project task named
//! `info` always shadows it (pass-through, any flags). Bare `runner`
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
    // isn't linkable from this integration crate — assert on the
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
fn project_task_named_info_always_shadows_the_deprecated_verb() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Without flags.
    let output = Command::new(runner_binary())
        .args(["--dir", fixture("info-shadowed").to_str().unwrap(), "info"])
        .output()
        .expect("runner binary spawns");

    assert!(
        output.status.success(),
        "shadowed `runner info` should exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("project-info-recipe-ran"),
        "the project `info` recipe should run. stdout: {stdout}",
    );
    assert!(
        !stderr.contains("deprecated"),
        "no deprecation warning when shadowed by a real task. stderr: {stderr}",
    );

    // Pass-through must ignore flags too.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("info-shadowed").to_str().unwrap(),
            "info",
            "--json",
        ])
        .output()
        .expect("runner binary spawns");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("project-info-recipe-ran"),
        "shadow pass-through must ignore --json. stdout: {stdout}",
    );
    assert!(
        !stderr.contains("deprecated"),
        "no deprecation warning when shadowed, even with --json. stderr: {stderr}",
    );
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
