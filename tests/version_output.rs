//! Integration coverage for version selectors, modifiers, and precedence.

use std::path::PathBuf;
use std::process::{Command, Output};

fn binary(name: &str) -> PathBuf {
    match name {
        "runner" => PathBuf::from(env!("CARGO_BIN_EXE_runner")),
        "run" => PathBuf::from(env!("CARGO_BIN_EXE_run")),
        _ => panic!("unknown test binary: {name}"),
    }
}

fn invoke(name: &str, args: &[&str]) -> Output {
    Command::new(binary(name))
        .args(args)
        .output()
        .expect("version binary spawns")
}

#[test]
fn run_version_after_global_option_is_detailed() {
    let output = invoke("run", &["--pm", "npm", "--version"]);

    assert!(
        output.status.success(),
        "run --pm npm --version should exit 0. stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for field in ["revision:", "target:", "profile:", "rustc:"] {
        assert!(
            stdout.lines().any(|line| line.starts_with(field)),
            "missing {field} field in stdout: {stdout}",
        );
    }
}

#[test]
fn revision_output_names_the_invoked_binary() {
    for name in ["runner", "run"] {
        let output = invoke(name, &["--revision"]);
        assert!(output.status.success(), "{name} --revision should succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with(&format!("{name} {}", env!("CARGO_PKG_VERSION"))),
            "revision output must name {name}. stdout: {stdout}",
        );
        assert!(
            stdout.contains('+'),
            "revision output needs metadata: {stdout}"
        );
    }
}

#[test]
fn detailed_modes_support_json_and_name_the_binary() {
    for (name, selector) in [("runner", "--version"), ("run", "--build-options")] {
        let output = invoke(name, &[selector, "--json"]);
        assert!(
            output.status.success(),
            "{name} JSON version should succeed"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("structured version JSON");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["program"], name);
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert!(value["target_config"]["features"].is_array());
    }
}

#[test]
fn quiet_modifiers_select_concise_output() {
    for (name, args) in [
        ("run", &["--version", "--quiet"][..]),
        ("run", &["-q", "--build-options"][..]),
        ("runner", &["--revision", "-qq"][..]),
    ] {
        let output = invoke(name, args);
        assert!(output.status.success(), "{name} {args:?} should succeed");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("{name} {}\n", env!("CARGO_PKG_VERSION")),
        );
    }
}

#[test]
fn json_is_rejected_for_non_detailed_modes() {
    for args in [["--revision", "--json"], ["-v", "--json"]] {
        let output = invoke("runner", &args);
        assert!(!output.status.success(), "runner {args:?} must fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cannot be used with '--json'"),
            "clap must report the selector/JSON conflict. stderr: {stderr}",
        );
    }
}

#[test]
fn version_does_not_hide_unknown_arguments() {
    for args in [
        &["--version", "--build-outputs"][..],
        &["--version", "--build-outputs", "-H"][..],
    ] {
        let output = invoke("runner", args);
        assert!(!output.status.success(), "runner {args:?} must fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--build-outputs"),
            "error must name the unknown argument. stderr: {stderr}",
        );
    }
}

#[test]
fn version_does_not_hide_commands_tasks_or_chain_options() {
    for (name, args) in [
        ("runner", &["--version", "extra"][..]),
        ("run", &["--version", "build"][..]),
        ("run", &["--version", "--sequential"][..]),
    ] {
        let output = invoke(name, args);
        assert!(!output.status.success(), "{name} {args:?} must fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("a version selector cannot be used with"),
            "error must report the semantic conflict. stderr: {stderr}",
        );
    }
}

#[test]
fn help_wins_before_a_task_even_with_invalid_version_arguments() {
    for name in ["runner", "run"] {
        let output = invoke(name, &["--version", "--build-outputs", "--help"]);
        assert!(output.status.success(), "{name} own help must win");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage:"), "missing help usage: {stdout}");
        assert!(
            stdout.contains("Version output:") || stdout.contains("Version flags:"),
            "missing version help: {stdout}",
        );
    }
}
