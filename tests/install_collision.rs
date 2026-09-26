#![cfg(unix)]

//! Integration tests for install-directory collisions.
//!
//! `runner install`'s multi-PM executor is exercised here against fake package
//! managers on `PATH`, shell scripts that log when they start and finish and
//! sleep in between. One manager installs a shared `node_modules/`, and
//! managers with their own directories run alongside it.

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

/// A project outside the repo tree (so detection can't ascend into the parent
/// crate) that bun and Deno both claim, with Deno set to materialize a local
/// `node_modules/`. The caller removes the returned dir when done.
fn colliding_project(name: &str, extra_pms: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runner-collision-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fakebin")).expect("create project dir");

    std::fs::write(dir.join("package.json"), r#"{"name":"collide"}"#).expect("package.json");
    std::fs::write(dir.join("bun.lock"), "").expect("bun.lock");
    std::fs::write(dir.join("deno.json"), r#"{"nodeModulesDir":"manual"}"#).expect("deno.json");
    if extra_pms.contains(&"cargo") {
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"collide\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .expect("Cargo.toml");
        std::fs::write(dir.join("src/lib.rs"), "").expect("lib.rs");
    }

    for pm in ["bun", "deno"].iter().chain(extra_pms) {
        fake_pm(&dir, pm);
    }
    dir
}

/// A package manager that records the window it was running in. `install`
/// brackets a 300ms sleep with `<pm> start` / `<pm> end` lines, so overlapping
/// installs interleave in the log and serialized ones cannot.
fn fake_pm(dir: &Path, pm: &str) {
    let script = dir.join("fakebin").join(pm);
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 1.0.0; exit 0; fi\necho '{pm} \
             start' >> \"$RUNNER_TEST_LOG\"\nsleep 0.3\necho '{pm} end' >> \"$RUNNER_TEST_LOG\"\n"
        ),
    )
    .expect("write fake pm");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake pm");
    }
}

fn install_in(dir: &Path, env: &[(&str, &str)]) -> (Output, String) {
    let log = dir.join("install.log");
    let _ = std::fs::remove_file(&log);
    let mut paths = vec![dir.join("fakebin")];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    let path = std::env::join_paths(paths).expect("test PATH entries are valid");
    let mut cmd = support::command(runner_binary());
    cmd.arg("install")
        .current_dir(dir)
        .env("PATH", path)
        .env("RUNNER_TEST_LOG", &log);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("run runner install");
    let log = std::fs::read_to_string(&log).unwrap_or_default();
    (output, log)
}

#[test]
fn only_the_resolved_writer_installs_the_shared_tree() {
    let dir = colliding_project("resolve", &[]);
    let (output, log) = install_in(&dir, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["bun start", "bun end"],
        "bun.lock resolves node_modules to bun, so deno must not install: {log}",
    );
    assert!(
        stderr.contains("deno shadowed"),
        "a skipped install must be said out loud: {stderr}",
    );
}

#[test]
fn no_warnings_keeps_the_shadowed_installer_notice() {
    let dir = colliding_project("shadow-notice", &[]);
    let (output, _) = install_in(&dir, &[("RUNNER_WARNINGS", "0")]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    assert!(
        stderr.contains("deno shadowed"),
        "a skipped installer is a plan notice, not a suppressible warning: {stderr}",
    );
}

#[test]
fn quiet_hides_runner_install_text_but_runs_installer() {
    let dir = colliding_project("quiet", &[]);
    let (output, log) = install_in(&dir, &[("RUNNER_QUIET", "4")]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["bun start", "bun end"]
    );
    assert_eq!(output.stdout.len(), 0);
    assert!(output.stderr.is_empty(), "stderr: {stderr}");
}

#[test]
fn managers_with_their_own_install_dirs_still_overlap() {
    let dir = colliding_project("parallel", &["cargo"]);
    let (output, log) = install_in(&dir, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    let lines: Vec<&str> = log.lines().collect();
    let cargo_start = lines
        .iter()
        .position(|l| *l == "cargo start")
        .unwrap_or_else(|| panic!("cargo never ran: {log}"));
    let bun_end = lines
        .iter()
        .position(|l| *l == "bun end")
        .unwrap_or_else(|| panic!("bun never finished: {log}"));
    assert!(
        cargo_start < bun_end,
        "cargo must overlap the node_modules lane, not queue behind it: {log}",
    );
    assert!(!log.contains("deno"), "deno is shadowed: {log}");
}

/// bun and npm lockfiles side by side, where npm reaches the install set
/// through the core resolver alone.
fn two_lockfile_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runner-collision-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fakebin")).expect("create project dir");
    std::fs::write(dir.join("package.json"), r#"{"name":"collide"}"#).expect("package.json");
    std::fs::write(dir.join("bun.lock"), "").expect("bun.lock");
    std::fs::write(dir.join("package-lock.json"), "{}").expect("package-lock.json");
    for pm in ["bun", "npm"] {
        fake_pm(&dir, pm);
    }
    dir
}

#[test]
fn a_second_lockfile_writer_is_shadowed() {
    let dir = two_lockfile_project("resolver-writer");
    let (output, log) = install_in(&dir, &[]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    assert_eq!(
        log.lines().count(),
        2,
        "one writer owns node_modules: {log}"
    );
    assert!(stderr.contains("shadowed"), "stderr: {stderr}");
}
