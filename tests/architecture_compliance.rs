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
fn go_task_stamps_vcs_inside_a_checkout() {
    let fixture = Fixture::new();
    fixture.file("go.mod", "module example.com/auditgo\n\ngo 1.24\n");
    fixture.file("main.go", "package main\nfunc main() {}\n");
    std::fs::create_dir(fixture.0.join(".git")).unwrap();
    fixture.program("git");
    fixture.program("go");
    fixture.file(
        "bin/go",
        "#!/bin/sh\ncase \"$1\" in\n version) echo 'go version go1.24.0 linux/amd64';;\n run) \
         printf '%s\\n' \"$2\" >> \"$AUDIT_LOG\";;\nesac\n",
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
fn cli_runs_an_explicit_shell_command_through_the_host_rung() {
    let fixture = Fixture::new();
    let output = fixture.run(&["run", "/bin/sh", "-c", "printf '%s' 'a b'"], "local");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"a b");
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

fn builtin_command(fixture: &Fixture, alias: bool, args: &[&str]) -> Command {
    let mut command = Command::new(if alias {
        env!("CARGO_BIN_EXE_run")
    } else {
        env!("CARGO_BIN_EXE_runner")
    });
    command
        .env_clear()
        .env("HOME", &fixture.0)
        .env("PATH", fixture.0.join("bin"))
        .env("RUNNER_REACH", "local")
        .env("AUDIT_LOG", fixture.0.join("executed"))
        .current_dir(&fixture.0);
    if !alias {
        command.arg("run");
    }
    command.args(args);
    command
}

#[test]
fn both_entrypoints_execute_builtin_flags_without_alias_recursion() {
    let fixture = Fixture::new();
    for alias in [false, true] {
        let output = builtin_command(&fixture, alias, &["list", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            json["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| task["name"] == "first")
        );
        let output = builtin_command(&fixture, alias, &["list", "--not-a-real-flag"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    }
    fixture.assert_not_executed();
}

#[test]
fn both_entrypoints_can_run_builtins_in_parallel() {
    let fixture = Fixture::new();
    for grouped in [false, true] {
        fixture.file("runner.toml", &format!("[parallel]\ngrouped = {grouped}\n"));
        for alias in [false, true] {
            let output = builtin_command(&fixture, alias, &["-p", "list", "first", "info"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            if grouped {
                let block = stdout
                    .split("runner: ")
                    .find(|block| block.starts_with("list\n"))
                    .unwrap_or_else(|| panic!("no list block: {stdout}"));
                assert!(block.contains("first"), "{stdout}");
            } else {
                assert!(
                    stdout
                        .lines()
                        .any(|line| line.starts_with("[list") && line.contains("first")),
                    "{stdout}"
                );
                assert!(
                    stdout
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .all(|line| line.starts_with('[')),
                    "{stdout}"
                );
            }
            let logged = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
            assert_eq!(logged.trim(), "run first");
            std::fs::remove_file(fixture.0.join("executed")).unwrap();
        }
    }
}

#[test]
fn a_parallel_chain_refuses_the_install_builtin() {
    let fixture = Fixture::new();
    for alias in [false, true] {
        let output = builtin_command(&fixture, alias, &["-p", "install", "list"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("install items cannot run in parallel chains"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fixture.assert_not_executed();
}

#[test]
fn builtin_explanations_render_without_running_or_cleaning() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.0.join("node_modules")).unwrap();
    fixture.file("node_modules/keep", "keep");
    for alias in [false, true] {
        for args in [
            vec!["--explain", "list"],
            vec!["--explain", "list", "--json"],
            vec!["--explain", "clean", "--yes"],
            vec!["--explain", "-p", "list", "info"],
        ] {
            let output = builtin_command(&fixture, alias, &args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("execution: in-process"),
                "{args:?}"
            );
            assert!(output.stdout.is_empty(), "builtin body must not run");
            assert!(fixture.0.join("node_modules/keep").is_file());
        }
    }
    fixture.assert_not_executed();
}

#[test]
fn cli_preserves_explicit_shell_command_flags() {
    let fixture = Fixture::new();
    fixture.program("bash");
    for args in [
        vec!["run", "bash", "-lc", "echo harmless"],
        vec!["run", "bash", "-e", "-c", "echo harmless"],
    ] {
        let output = fixture.run(&args, "local");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let executed = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
        assert!(executed.contains("echo harmless"), "{executed}");
    }
}

#[test]
fn cli_refuses_node_jsx_but_allows_capable_runtimes() {
    let fixture = Fixture::new();
    for tool in ["node", "bun", "deno"] {
        fixture.program(tool);
    }
    for extension in ["jsx", "tsx"] {
        let file = format!("view.{extension}");
        fixture.file(&file, "const view = <div />;\n");
        for runtime in [None, Some("node")] {
            let mut args = vec!["run"];
            if let Some(runtime) = runtime {
                args.extend(["--runtime", runtime]);
            }
            args.push(&file);
            let output = fixture.run(&args, "local");
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("node cannot run"), "{stderr}");
            for label in ["bun", "deno"] {
                assert!(stderr.contains(&format!("--runtime {label}")), "{stderr}");
            }
            fixture.assert_not_executed();
        }
        for runtime in ["bun", "deno"] {
            let output = fixture.run(&["run", "--runtime", runtime, &file], "local");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let logged = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
            assert!(logged.contains(&file));
            if runtime == "deno" {
                assert!(!logged.contains("--allow") && !logged.contains("-A"));
            }
            std::fs::remove_file(fixture.0.join("executed")).unwrap();
        }
    }
}

#[test]
fn cli_yarn_observation_errors_cannot_execute_classic_fallback() {
    for name in ["package.json", "yarn.lock"] {
        let fixture = Fixture::new();
        fixture.program("yarn");
        fixture.file("package.json", r#"{"packageManager":"yarn@4.0.0"}"#);
        fixture.file("yarn.lock", "__metadata:\n  version: 8\n");
        std::fs::remove_file(fixture.0.join(name)).unwrap();
        std::fs::create_dir(fixture.0.join(name)).unwrap();
        for args in [vec!["run", "missing-bin"], vec!["why", "missing-bin"]] {
            let output = fixture.run(&args, "local");
            let message = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                message.contains("yarn observation failed") && message.contains(name),
                "{message}"
            );
            if args[0] == "run" {
                assert!(!output.status.success());
            }
            fixture.assert_not_executed();
        }
    }
}

#[test]
fn cli_script_files_keep_interpreter_and_forwarded_argument_boundaries() {
    let fixture = Fixture::new();
    fixture.file("script.sh", "#!/bin/sh -e\nprintf '%s\\n' \"$@\"\n");
    let output = fixture.run(
        &["run", "./script.sh", "-c", "argument with spaces"],
        "local",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "-c\nargument with spaces\n"
    );
    fixture.assert_not_executed();
}

#[test]
fn runtime_override_handles_node_shebangs_and_preserves_shell_scripts() {
    let fixture = Fixture::new();
    fixture.program("bun");
    for name in ["entry.js", "entry"] {
        fixture.file(name, "#!/usr/bin/env node\nconsole.log('unused');\n");
        std::fs::set_permissions(fixture.0.join(name), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let token = format!("./{name}");
        let output = fixture.run(&["run", "--runtime", "bun", &token, "two words"], "local");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let logged = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
        assert!(
            logged.contains(name) && logged.contains("two words"),
            "{logged}"
        );
        std::fs::remove_file(fixture.0.join("executed")).unwrap();
    }
    fixture.file("shell.sh", "#!/bin/sh\nprintf shell-ok\n");
    let output = fixture.run(&["run", "--runtime", "bun", "./shell.sh"], "local");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"shell-ok");
    fixture.assert_not_executed();
}

#[test]
fn jsx_refusal_identifies_cli_and_environment_runtime_origins() {
    let fixture = Fixture::new();
    fixture.file("view.jsx", "const view = <div />;\n");
    let cli = fixture.run(&["run", "--runtime", "node", "view.jsx"], "local");
    assert!(!cli.status.success());
    let message = String::from_utf8_lossy(&cli.stderr);
    assert!(
        message.contains("selected by --runtime") && message.contains("--runtime bun"),
        "{message}"
    );
    let env = builtin_command(&fixture, false, &["view.jsx"])
        .env("RUNNER_RUNTIME", "node")
        .output()
        .unwrap();
    assert!(!env.status.success());
    let message = String::from_utf8_lossy(&env.stderr);
    assert!(message.contains("selected by RUNNER_RUNTIME"), "{message}");
    fixture.assert_not_executed();
}

#[test]
fn execute_only_source_uses_its_runtime() {
    let fixture = Fixture::new();
    fixture.program("node");
    fixture.file("entry.js", "console.log('unused');\n");
    std::fs::set_permissions(
        fixture.0.join("entry.js"),
        std::fs::Permissions::from_mode(0o111),
    )
    .unwrap();
    for token in ["entry.js", "./entry.js"] {
        let output = fixture.run(&["run", token], "local");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let logged = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
        assert_eq!(logged.trim(), fixture.0.join("entry.js").to_string_lossy());
        std::fs::remove_file(fixture.0.join("executed")).unwrap();
    }
}

#[test]
fn builtin_arguments_preserve_parent_reach_policy() {
    let fixture = Fixture::new();
    for alias in [false, true] {
        let output = builtin_command(
            &fixture,
            alias,
            &["--fetch", "local", "install", "--no-tools"],
        )
        .env("RUNNER_REACH", "allow")
        .output()
        .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("reach policy"));
        fixture.assert_not_executed();
    }
}

#[test]
fn builtin_install_applies_its_script_flags() {
    let fixture = Fixture::new();
    for alias in [false, true] {
        let output = builtin_command(
            &fixture,
            alias,
            &["--fetch", "allow", "install", "--no-tools", "--no-scripts"],
        )
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let logged = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
        assert!(logged.contains("--ignore-scripts"), "{logged}");
        std::fs::remove_file(fixture.0.join("executed")).unwrap();
    }
}

#[test]
fn removed_internal_flag_is_rejected() {
    let fixture = Fixture::new();
    let output = fixture.run(&["--internal-runner-builtin", "list"], "local");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    fixture.assert_not_executed();
}

#[test]
fn yarn_install_preview_and_execution_share_the_registry_variant() {
    for (version, frozen, verb, script_env) in [
        ("1.22.0", "--frozen-lockfile", "run", ""),
        ("4.1.0", "--immutable", "exec", "false"),
    ] {
        let fixture = Fixture::new();
        fixture.file(
            "package.json",
            &format!(r#"{{"packageManager":"yarn@{version}","scripts":{{"build":"echo build"}}}}"#),
        );
        std::fs::remove_file(fixture.0.join("package-lock.json")).unwrap();
        fixture.file("yarn.lock", "");
        fixture.program("yarn");
        fixture.file(
            "bin/yarn",
            "#!/bin/sh\nprintf '%s|%s\\n' \"$YARN_ENABLE_SCRIPTS\" \"$*\" >> \"$AUDIT_LOG\"\n",
        );
        let explained = fixture.run(
            &[
                "--explain",
                "install",
                "--no-tools",
                "--frozen",
                "--no-scripts",
            ],
            "local",
        );
        assert!(
            explained.status.success(),
            "{}",
            String::from_utf8_lossy(&explained.stderr)
        );
        let stderr = String::from_utf8_lossy(&explained.stderr);
        assert!(
            stderr.contains(frozen) && stderr.contains("evidence:") && stderr.contains("Network"),
            "{stderr}"
        );
        fixture.assert_not_executed();
        let installed = fixture.run(
            &["install", "--no-tools", "--frozen", "--no-scripts"],
            "allow",
        );
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let log = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
        assert!(
            log.starts_with(&format!("{script_env}|install {frozen}")),
            "{log}"
        );
        assert_eq!(log.contains("--ignore-scripts"), version.starts_with('1'));
        let explained = fixture.run(&["--explain", "run", "missing-widget"], "local");
        assert!(
            String::from_utf8_lossy(&explained.stderr).contains(&format!("\"yarn\", \"{verb}\""))
        );
        let why = fixture.run(&["why", "build", "--json"], "local");
        assert!(
            why.status.success(),
            "{}",
            String::from_utf8_lossy(&why.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&why.stdout).unwrap();
        assert_eq!(
            report["selected"]["task"]["resolved"],
            if version.starts_with('1') {
                "yarn build"
            } else {
                "yarn run build"
            }
        );
    }
}

#[test]
fn tool_install_refreshes_declared_bin_paths_before_the_package_manager_starts() {
    let fixture = Fixture::new();
    fixture.file("mise.toml", "[tools]\n");
    fixture.program("mise");
    std::fs::create_dir(fixture.0.join("managed")).unwrap();
    std::fs::rename(fixture.0.join("bin/npm"), fixture.0.join("managed/npm")).unwrap();
    fixture.file(
        "bin/mise",
        r#"#!/bin/sh
case "$1" in
  tasks) echo '{}';;
  --version) echo 2026.1.0;;
  bin-paths) if [ -f "$HOME/installed" ]; then printf '%s/managed\n' "$HOME"; fi;;
  install) printf installed > "$HOME/installed";;
esac
"#,
    );
    let output = fixture.run(&["install"], "allow");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("executed"))
            .unwrap()
            .trim(),
        "install"
    );
}

#[test]
fn install_refuses_broken_yarn_observations_before_spawning() {
    let fixture = Fixture::new();
    fixture.file("package.json", r#"{"packageManager":"yarn@4.1.0"}"#);
    std::fs::remove_file(fixture.0.join("package-lock.json")).unwrap();
    std::fs::create_dir(fixture.0.join("yarn.lock")).unwrap();
    fixture.program("yarn");
    let output = fixture.run(&["install", "--no-tools", "--frozen"], "allow");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("yarn.lock"));
    fixture.assert_not_executed();
}

#[test]
fn observed_runtime_keeps_precedence_for_package_exec() {
    let fixture = Fixture::new();
    fixture.file("bun.lock", "");
    fixture.program("bun");
    let output = fixture.run(
        &["--runtime", "bun", "--explain", "run", "audit-missing"],
        "allow",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("[\"bun\", \"x\", \"--bun\", \"audit-missing\"]"),
        "{stderr}"
    );
    fixture.assert_not_executed();
}

#[test]
fn member_bin_precedes_root_bin() {
    let fixture = Fixture::new();
    fixture.file(
        "package.json",
        r#"{"name":"root","packageManager":"npm@11.0.0","workspaces":["packages/*"]}"#,
    );
    let member = fixture.0.join("packages/web");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(member.join("package.json"), r#"{"name":"web"}"#).unwrap();
    for (root, value) in [(&fixture.0, "root"), (&member, "member")] {
        let bin = root.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join("audit-local");
        std::fs::write(&path, format!("#!/bin/sh\necho {value}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = fixture.run(
        &["--dir", member.to_str().unwrap(), "run", "audit-local"],
        "local",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "member");
}

#[test]
fn qualified_task_environment_overrides_the_bare_task() {
    let fixture = Fixture::new();
    fixture.file(
        "runner.toml",
        "[tasks.first.env]\nAUDIT_VALUE = \
         'bare'\n[tasks.\"root:package.json#first\".env]\nAUDIT_VALUE = 'qualified'\n",
    );
    fixture.file(
        "bin/npm",
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 11.0.0; else printf '%s' \
         \"$AUDIT_VALUE\"; fi\n",
    );
    let output = fixture.run(&["run", "root:package.json#first"], "local");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"qualified");
}

#[test]
fn why_and_run_both_refuse_an_old_node_task_runtime() {
    let fixture = Fixture::new();
    fixture.program("node");
    fixture.file("bin/node", "#!/bin/sh\necho v20.0.0\n");
    for verb in ["why", "run"] {
        let output = fixture.run(&["--runtime", "node", verb, "first"], "local");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(text.contains("needs Node 22 or newer"), "{verb}: {text}");
        assert!(!text.contains("Invalid("), "{text}");
    }
    fixture.assert_not_executed();
}

#[test]
fn path_search_skips_a_nonexecutable_file() {
    let fixture = Fixture::new();
    fixture.file("bin/audit-host", "not executable");
    let later = fixture.0.join("later");
    std::fs::create_dir_all(&later).unwrap();
    let path = later.join("audit-host");
    std::fs::write(&path, "#!/bin/sh\necho executable\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_runner"))
        .env_clear()
        .env("HOME", &fixture.0)
        .env(
            "PATH",
            std::env::join_paths([fixture.0.join("bin"), later]).unwrap(),
        )
        .env("RUNNER_REACH", "local")
        .current_dir(&fixture.0)
        .args(["run", "audit-host"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "executable");
}

#[test]
fn unreadable_installed_package_never_becomes_a_fetch_plan() {
    let fixture = Fixture::new();
    let dir = fixture.0.join("node_modules/audit-dep");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir(dir.join("package.json")).unwrap();
    let output = fixture.run(
        &["--explain", "run", "--package", "audit-dep", "audit-dep"],
        "allow",
    );
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Network"));
    fixture.assert_not_executed();
}

#[test]
fn unrepresentable_project_path_never_hides_a_local_binary() {
    let fixture = Fixture::new();
    let moved = fixture.0.with_file_name(format!(
        "{}:colon",
        fixture.0.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(&fixture.0, &moved).unwrap();
    let fixture = Fixture(moved);
    let bin = fixture.0.join("node_modules/.bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("audit-local");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture.run(&["--explain", "run", "audit-local"], "allow");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("cannot construct the planned PATH"),
        "{stderr}"
    );
    assert!(!stderr.contains("npx"), "{stderr}");
    fixture.assert_not_executed();
}

#[test]
fn a_forced_install_without_files_or_executables_has_no_plan() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.0.join("package.json")).unwrap();
    std::fs::remove_file(fixture.0.join("package-lock.json")).unwrap();
    std::fs::remove_file(fixture.0.join("bin/npm")).unwrap();
    let output = fixture.run(
        &["--pm", "npm", "--explain", "install", "--no-tools"],
        "allow",
    );
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("argv:"));
    fixture.assert_not_executed();
}

#[test]
fn disabled_installers_do_not_request_network_or_execute() {
    let fixture = Fixture::new();
    fixture.file("mise.toml", "");
    fixture.file(
        "runner.toml",
        "[tools.npm]\ninstall = false\n[tools.mise]\ninstall = false\n",
    );
    fixture.program("mise");
    fixture.file(
        "bin/mise",
        "#!/bin/sh\ncase \"$1\" in tasks) echo '{}';; bin-paths) :;; *) echo ran >> \
         \"$AUDIT_LOG\";; esac\n",
    );
    let output = fixture.run(&["install"], "local");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_not_executed();
}

#[test]
fn plug_n_play_packages_resolve_in_the_invoking_member() {
    let fixture = Fixture::new();
    fixture.file(
        "package.json",
        r#"{"name":"root","private":true,"packageManager":"yarn@4.0.0","workspaces":["packages/*"]}"#,
    );
    std::fs::remove_file(fixture.0.join("package-lock.json")).unwrap();
    fixture.file("yarn.lock", "");
    fixture.file(".pnp.cjs", "");
    let member = fixture.0.join("packages").join("web");
    std::fs::create_dir_all(&member).unwrap();
    fixture.file("packages/web/package.json", r#"{"name":"web"}"#);
    let yarn = fixture.0.join("bin").join("yarn");
    std::fs::write(
        &yarn,
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 4.0.0;;\n bin) printf 'bin %s\\n' \"$(pwd \
         -P)\" >> \"$AUDIT_LOG\"; echo \
         '{\"name\":\"tsc\",\"source\":\"typescript\",\"path\":\"/x/tsc\"}';;\n *) printf '%s\\n' \
         \"$*\" >> \"$AUDIT_LOG\";;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&yarn, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_runner"))
        .env_clear()
        .env("PATH", fixture.0.join("bin"))
        .env("HOME", &fixture.0)
        .env("AUDIT_LOG", fixture.0.join("executed"))
        .env("RUNNER_REACH", "local")
        .current_dir(&member)
        .args(["run", "--package", "typescript", "tsc"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged = std::fs::read_to_string(fixture.0.join("executed")).unwrap();
    assert!(
        logged
            .lines()
            .any(|line| line == format!("bin {}", member.canonicalize().unwrap().display())),
        "{logged}"
    );
}
