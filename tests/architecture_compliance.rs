#![cfg(unix)]

//! Execution boundaries exercised with harmless fake host tools.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "runner-architecture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let fixture = Self(root);
        fixture.file("package.json", r#"{"name":"audit","private":true,"packageManager":"npm@11.0.0","scripts":{"first":"echo first","second":"echo second"}}"#);
        fixture.file("package-lock.json", "{}");
        fixture.program("npm");
        fixture
    }

    fn file(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(name), text).unwrap();
    }

    fn program(&self, name: &str) {
        let path = self.0.join("bin").join(name);
        std::fs::write(
            &path,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 11.0.0; exit 0; fi\nprintf '%s\\n' \
             \"$*\" >> \"$AUDIT_LOG\"\n",
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str], reach: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_runner"));
        command
            .env_clear()
            .env("PATH", self.0.join("bin"))
            .env("HOME", &self.0)
            .env("AUDIT_LOG", self.0.join("executed"))
            .env("RUNNER_REACH", reach)
            .current_dir(&self.0)
            .args(args);
        command.output().unwrap()
    }

    fn assert_not_executed(&self) {
        assert!(
            !self.0.join("executed").exists(),
            "an execution subprocess ran"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn local_policy_refuses_install_before_any_package_manager_runs() {
    let fixture = Fixture::new();
    let output = fixture.run(&["install", "--no-tools"], "local");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reach policy"));
    fixture.assert_not_executed();
    let allowed = fixture.run(&["install", "--no-tools"], "allow");
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(fixture.0.join("executed").exists());
}

#[test]
fn explain_install_never_executes_even_with_multiple_package_managers() {
    let fixture = Fixture::new();
    fixture.file(
        "Cargo.toml",
        "[package]\nname = 'audit'\nversion = '0.0.0'\n",
    );
    fixture.program("cargo");
    let output = fixture.run(&["--explain", "install", "--no-tools"], "local");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("argv:") && stderr.contains("npm") && stderr.contains("cargo"),
        "{stderr}"
    );
    fixture.assert_not_executed();
}

#[test]
fn explain_never_executes_sequential_or_parallel_chains() {
    for mode in ["-s", "-p"] {
        let fixture = Fixture::new();
        let output = fixture.run(&["--explain", "run", mode, "first", "second"], "allow");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("first") && stderr.contains("second"),
            "{stderr}"
        );
        fixture.assert_not_executed();
    }
}

#[test]
fn explain_does_not_execute_or_authorize_a_network_fallback() {
    let fixture = Fixture::new();
    fixture.program("npx");
    let output = fixture.run(
        &["--explain", "run", "audit-package-not-installed"],
        "local",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Network"));
    fixture.assert_not_executed();
}

#[test]
fn selected_package_obeys_reach_and_explain() {
    let fixture = Fixture::new();
    fixture.program("npx");
    let output = fixture.run(&["run", "--package", "audit-missing", "audit-bin"], "local");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reach policy"));
    fixture.assert_not_executed();
    let output = fixture.run(
        &[
            "--explain",
            "run",
            "--package",
            "audit-missing",
            "audit-bin",
        ],
        "local",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_not_executed();
}

#[test]
fn explain_clean_keeps_every_target_even_with_yes() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("node_modules")).unwrap();
    fixture.file("node_modules/keep", "keep");
    let output = fixture.run(&["--explain", "clean", "--yes"], "allow");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("node_modules/keep")).unwrap(),
        "keep"
    );
}

#[test]
fn a_host_manager_plan_cannot_resolve_to_a_project_shim() {
    let fixture = Fixture::new();
    fixture.file("mise.toml", "[tools]\n");
    // Observation gets harmless, valid answers; only exec produces the marker.
    fixture.file(
        "bin/mise",
        "#!/bin/sh\ncase \"$1\" in\n tasks) echo '{}';;\n bin-paths) :;;\n exec) echo host >> \
         \"$AUDIT_LOG\";;\n *) echo 11.0.0;;\nesac\n",
    );
    std::fs::set_permissions(
        fixture.0.join("bin/mise"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::create_dir_all(fixture.0.join("node_modules/.bin")).unwrap();
    fixture.file(
        "node_modules/.bin/mise",
        "#!/bin/sh\necho project >> \"$AUDIT_LOG\"\n",
    );
    std::fs::set_permissions(
        fixture.0.join("node_modules/.bin/mise"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let output = fixture.run(&["run", "audit-package-not-installed"], "allow");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("executed")).unwrap(),
        "host\n"
    );
}

#[test]
fn go_task_keeps_vcs_stamping_in_the_planned_environment() {
    let fixture = Fixture::new();
    fixture.file("go.mod", "module example.com/auditgo\n\ngo 1.24\n");
    fixture.file("main.go", "package main\nfunc main() {}\n");
    std::fs::create_dir(fixture.0.join(".git")).unwrap();
    fixture.program("git");
    fixture.program("go");
    fixture.file(
        "bin/go",
        "#!/bin/sh\ncase \"$1\" in\n version) echo 'go version go1.24.0 linux/amd64';;\n run) \
         printf '%s\\n' \"$GOFLAGS\" >> \"$AUDIT_LOG\";;\nesac\n",
    );
    let output = fixture.run(&["run", "auditgo"], "local");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("executed")).unwrap(),
        "-buildvcs=true\n"
    );
}

#[test]
#[cfg(debug_assertions)]
fn cli_cannot_spawn_a_shell_command_string_through_the_host_rung() {
    let fixture = Fixture::new();
    fixture.program("sh");
    let output = fixture.run(&["run", "sh", "-c", "exit 0"], "allow");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("never a shell string"));
    fixture.assert_not_executed();
}

#[test]
fn cli_yarn_variant_uses_the_observed_package_manager_declaration() {
    for (version, verb) in [("1.22.22", "run"), ("4.0.0", "exec")] {
        let fixture = Fixture::new();
        fixture.file(
            "package.json",
            &format!(r#"{{"packageManager":"yarn@{version}"}}"#),
        );
        std::fs::remove_file(fixture.0.join("package-lock.json")).unwrap();
        fixture.file("yarn.lock", "");
        fixture.program("yarn");
        let output = fixture.run(&["run", "runner-local-variant-tool"], "local");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let executed = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
        assert_eq!(executed.trim(), format!("{verb} runner-local-variant-tool"));
    }
}
