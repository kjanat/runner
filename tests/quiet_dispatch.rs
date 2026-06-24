//! Integration coverage for `--quiet` / `RUNNER_QUIET`: the dispatch
//! arrow (`→ <source> <task>`) must stay off stderr when quiet is on.

use std::path::PathBuf;
use std::process::Command;

fn run_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_run"))
}

fn run_command() -> Command {
    let mut cmd = Command::new(run_binary());
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RUNNER_")
        {
            cmd.env_remove(&key);
        }
    }
    cmd
}

fn npx_available() -> bool {
    Command::new("npx")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn quiet_flag_suppresses_dispatch_arrow() {
    if !npx_available() {
        eprintln!("skipping: `npx` not found on PATH");
        return;
    }

    let output = run_command()
        .args(["--quiet", "npx", "--version"])
        .output()
        .expect("run should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() != Some(2),
        "run must reach dispatch (exit 2 = arg/resolve failure before dispatch). status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        !stderr.contains('→'),
        "dispatch arrow must be suppressed with --quiet. stderr: {stderr}",
    );
}

#[test]
fn runner_quiet_env_suppresses_dispatch_arrow() {
    if !npx_available() {
        eprintln!("skipping: `npx` not found on PATH");
        return;
    }

    let output = run_command()
        .env("RUNNER_QUIET", "1")
        .args(["npx", "--version"])
        .output()
        .expect("run should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() != Some(2),
        "run must reach dispatch (RUNNER_QUIET=1). status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        !stderr.contains('→'),
        "dispatch arrow must be suppressed with RUNNER_QUIET=1. stderr: {stderr}",
    );
}

#[test]
fn dispatch_arrow_prints_without_quiet() {
    if !npx_available() {
        eprintln!("skipping: `npx` not found on PATH");
        return;
    }

    let output = run_command()
        .args(["npx", "--version"])
        .output()
        .expect("run should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() != Some(2),
        "run must reach dispatch. status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        stderr.contains('→'),
        "dispatch arrow expected without --quiet. stderr: {stderr}",
    );
}

#[test]
fn quiet_with_explain_suppresses_dispatch_and_explain() {
    if !npx_available() {
        eprintln!("skipping: `npx` not found on PATH");
        return;
    }

    let output = run_command()
        .args(["--quiet", "--explain", "npx", "--version"])
        .output()
        .expect("run should execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() != Some(2),
        "run must reach dispatch (--quiet --explain). status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        !stderr.contains('→'),
        "dispatch arrow must be suppressed under --quiet. stderr: {stderr}",
    );
    assert!(
        !stderr.contains("resolved:"),
        "--explain trace must be suppressed under --quiet. stderr: {stderr}",
    );
}
