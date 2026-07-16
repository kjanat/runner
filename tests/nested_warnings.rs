//! A warning belongs to a project, not to a process.
//!
//! `"fmt": "runner run lint:fix fmt:dprint"` is an ordinary shape, and it means
//! a single `runner fmt` becomes two runner processes over one project. Both
//! used to print the same detection warnings.

use std::path::PathBuf;
use std::process::Command;

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn just_available() -> bool {
    Command::new("just")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A project whose `runner.toml` carries an unknown key (a warning every
/// command prints) and whose `outer` task invokes `runner` again.
fn nesting_project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runner-nested-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::write(dir.join("runner.toml"), "[bogus]\nkey = 1\n").expect("runner.toml");
    std::fs::write(
        dir.join("justfile"),
        "outer:\n\t@runner run inner\n\ninner:\n\t@echo inner-ran\n",
    )
    .expect("justfile");
    dir
}

#[test]
fn a_nested_runner_does_not_repeat_the_warnings_its_parent_printed() {
    if !just_available() {
        eprintln!("skipping: `just` not on PATH");
        return;
    }
    let dir = nesting_project();
    let bin_dir = runner_binary()
        .parent()
        .expect("binary lives in a directory")
        .to_path_buf();
    let mut paths = vec![bin_dir];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    let path = std::env::join_paths(paths).expect("test PATH entries are valid");

    let output = Command::new(runner_binary())
        .arg("run")
        .arg("outer")
        .current_dir(&dir)
        .env("PATH", path)
        .env_remove("RUNNER_WARNED_ROOT")
        .env_remove("RUNNER_NO_WARNINGS")
        .output()
        .expect("run runner");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "run failed: {stderr}");
    assert!(
        stdout.contains("inner-ran"),
        "the nested runner must still run the task: {stdout}",
    );
    assert_eq!(
        stderr.matches("unknown key").count(),
        1,
        "the same warning, once per project, not once per process: {stderr}",
    );
}

#[test]
fn a_nested_runner_over_a_different_root_still_warns() {
    // The marker is keyed on the root: a runner pointed somewhere else has its
    // own detection to report, and silence there would be a bug of its own.
    let dir = nesting_project();
    let elsewhere =
        std::env::temp_dir().join(format!("runner-nested-other-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(&elsewhere).expect("create other dir");
    std::fs::write(elsewhere.join("runner.toml"), "[nonsense]\nkey = 1\n").expect("runner.toml");

    let output = Command::new(runner_binary())
        .arg("list")
        .current_dir(&elsewhere)
        .env("RUNNER_WARNED_ROOT", &dir)
        .env_remove("RUNNER_NO_WARNINGS")
        .output()
        .expect("run runner");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);

    assert_eq!(
        stderr.matches("unknown key").count(),
        1,
        "a marker for another project must not silence this one: {stderr}",
    );
}
