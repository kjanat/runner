//! Integration tests for chain mode dispatch.
//!
//! Each test spawns the `runner` binary against a fixture project under
//! `tests/fixtures/`. The fixtures use `just` (already a dependency the
//! repo expects to be on PATH for development) so the tests don't need
//! to install any package managers.
//!
//! If `just` is not installed, the integration tests skip with a
//! warning rather than failing. Run them locally with `cargo test
//! --test chain_integration`.

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
fn sequential_chain_runs_in_order() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "-s",
            "build",
            "test",
            "lint",
        ])
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success.\nstdout: {stdout}\nstderr: {stderr}",
    );

    let b = stdout
        .find("build-ran")
        .unwrap_or_else(|| panic!("build-ran missing.\nstdout: {stdout}"));
    let t = stdout
        .find("test-ran")
        .unwrap_or_else(|| panic!("test-ran missing.\nstdout: {stdout}"));
    let l = stdout
        .find("lint-ran")
        .unwrap_or_else(|| panic!("lint-ran missing.\nstdout: {stdout}"));
    assert!(b < t && t < l, "order should match -s arg order: {stdout}");
}

#[test]
fn parallel_chain_exit_code_reflects_first_failure() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-parallel-fail").to_str().unwrap(),
            "run",
            "-p",
            "ok-one",
            "fail-mid",
            "ok-two",
        ])
        .output()
        .expect("runner binary spawns");

    assert_eq!(
        output.status.code(),
        Some(7),
        "expected exit 7 from fail-mid task.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parallel output should be line-prefixed.
    assert!(
        stdout.contains("[ok-one"),
        "expected `[ok-one ]` prefix on ok-one's output. stdout: {stdout}",
    );
    assert!(
        stdout.contains("[fail-mid"),
        "expected `[fail-mid]` prefix on fail-mid's output. stdout: {stdout}",
    );
}

#[test]
fn chain_rejects_mutually_exclusive_mode_flags() {
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "-s",
            "-p",
            "build",
        ])
        .output()
        .expect("runner binary spawns");

    assert!(
        !output.status.success(),
        "expected clap to reject -s + -p combo",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--sequential") || stderr.contains("--parallel"),
        "expected clap conflict diagnostic. stderr: {stderr}",
    );
}

#[test]
fn chain_rejects_whitespace_positional_in_v1() {
    // No `just_available()` gate — the parser rejects the whitespace
    // positional before any task is dispatched, so the test runs
    // regardless of whether `just` is on PATH.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "-s",
            "build --release",
        ])
        .output()
        .expect("runner binary spawns");

    assert!(
        !output.status.success(),
        "v1 rejects whitespace positionals",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("whitespace") || stderr.contains("quoted-bundle"),
        "expected v1 rejection diagnostic. stderr: {stderr}",
    );
}
