//! Integration coverage for version requests after leading global options.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn run_version_after_global_option_is_detailed() {
    let output = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_run")))
        .args(["--pm", "npm", "--version"])
        .output()
        .expect("run binary spawns");

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
