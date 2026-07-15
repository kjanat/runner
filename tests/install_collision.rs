//! Integration tests for install-directory collisions.
//!
//! `runner install`'s multi-PM executor is exercised here against fake package
//! managers on `PATH`, shell scripts that log when they start and finish and
//! sleep in between. That makes the two properties that matter observable:
//! managers sharing `node_modules/` never overlap, and managers that don't
//! share a directory still do.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    let path = format!(
        "{}:{}",
        dir.join("fakebin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new(runner_binary());
    cmd.arg("install")
        .current_dir(dir)
        .env("PATH", path)
        .env("RUNNER_TEST_LOG", &log)
        .env_remove("RUNNER_INSTALL_PMS")
        .env_remove("RUNNER_INSTALL_ON_COLLISION");
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
fn naming_both_writers_runs_them_one_after_another() {
    let dir = colliding_project("consent", &[]);
    let (output, log) = install_in(&dir, &[("RUNNER_INSTALL_PMS", "bun,deno")]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    // Not merely "both ran": bun must be *finished* before deno starts, or the
    // two are writing one node_modules at once.
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec!["bun start", "bun end", "deno start", "deno end"],
        "writers of one tree must not overlap: {log}",
    );
    assert!(
        stderr.contains("all install into node_modules/"),
        "the redundant second install still warrants a warning: {stderr}",
    );
}

#[test]
fn managers_with_their_own_install_dirs_still_overlap() {
    let dir = colliding_project("parallel", &["cargo"]);
    let (output, log) = install_in(&dir, &[("RUNNER_INSTALL_PMS", "bun,deno,cargo")]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(output.status.success(), "install failed: {stderr}");
    let lines: Vec<&str> = log.lines().collect();
    // cargo writes `target/`, so it has no reason to wait for the node_modules
    // lane: it must start before that lane has finished.
    let cargo_start = lines
        .iter()
        .position(|l| *l == "cargo start")
        .unwrap_or_else(|| panic!("cargo never ran: {log}"));
    let deno_end = lines
        .iter()
        .position(|l| *l == "deno end")
        .unwrap_or_else(|| panic!("deno never finished: {log}"));
    assert!(
        cargo_start < deno_end,
        "cargo must overlap the node_modules lane, not queue behind it: {log}",
    );
    let bun_end = lines.iter().position(|l| *l == "bun end").expect("bun ran");
    let deno_start = lines
        .iter()
        .position(|l| *l == "deno start")
        .expect("deno ran");
    assert!(
        bun_end < deno_start,
        "the two node_modules writers still must not overlap: {log}",
    );
}

#[test]
fn on_collision_error_installs_nothing() {
    let dir = colliding_project("error", &[]);
    let (output, log) = install_in(&dir, &[("RUNNER_INSTALL_ON_COLLISION", "error")]);
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        log.is_empty(),
        "refusing to pick means refusing to install, not installing first: {log}",
    );
    assert!(stderr.contains("node_modules"), "stderr: {stderr}");
}
