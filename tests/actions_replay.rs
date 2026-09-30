//! Actions output must remain usable as data without quiet flags.
#![cfg(unix)]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;

struct Project(tempfile::TempDir);

impl Project {
    fn new(config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("runner.toml"), config).unwrap();
        Self(dir)
    }

    fn script(&self, name: &str, body: &str) {
        let file = self.0.path().join(name);
        std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = support::command(env!("CARGO_BIN_EXE_runner"));
        command
            .arg("--dir")
            .arg(self.0.path())
            .arg("run")
            .args(args)
            .env("GITHUB_ACTIONS", "true")
            .env("NO_COLOR", "1");
        command
    }
}

/// Check the matching resume marker and return bytes printed while commands
/// were suspended, plus anything emitted after command processing resumed.
fn suspended_body(output: &str) -> (&str, &str) {
    let (opening, rest) = output.split_once('\n').unwrap();
    let token = opening.strip_prefix("::stop-commands::").unwrap();
    assert_ne!(token, "");
    assert_ne!(
        token, "token",
        "must not reuse a token from captured output"
    );
    let (body, tail) = rest.split_once(&format!("::{token}::\n")).unwrap();
    assert!(body.is_empty() || body.ends_with('\n'));
    (body, tail)
}

#[test]
fn one_failure_replays_plain_after_live_stderr_and_keeps_stdout_exact() {
    let project = Project::new("");
    project.script(
        "fail.sh",
        "printf '{\"ok\":false}'; printf 'failed here\\n' >&2; exit 7",
    );
    let output = project.command(&["./fail.sh"]).output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, br#"{"ok":false}"#);
    let err = String::from_utf8(output.stderr).unwrap();
    assert_eq!(err.matches("failed here").count(), 2, "{err}");
    let start = err.find("::stop-commands::").expect(&err);
    let (body, tail) = suspended_body(&err[start..]);
    assert_eq!(body, "./fail.sh — exit 7\nfailed here\n");
    assert_eq!(tail, "");
    assert!(!err.contains("::group::"), "{err}");
}

#[test]
fn parallel_and_sequential_failures_replay_only_after_all_live_output() {
    for mode in ["-pk", "-sk"] {
        let project = Project::new("");
        project.script("a.sh", "printf 'a error\\n' >&2; exit 2");
        project.script("b.sh", "printf 'b error\\n' >&2; exit 3");
        let output = project
            .command(&[mode, "./a.sh", "./b.sh"])
            .output()
            .unwrap();
        assert!(matches!(output.status.code(), Some(2 | 3)));
        assert!(output.stdout.is_empty(), "{:?}", output.stdout);
        let err = String::from_utf8(output.stderr).unwrap();
        let first_group = err.find("::group::").expect(&err);
        assert!(err.find("a error").unwrap() < first_group, "{err}");
        assert!(err.find("b error").unwrap() < first_group, "{err}");
        assert_eq!(err.matches("::group::").count(), 2, "{err}");
        assert_eq!(err.matches("::endgroup::").count(), 2, "{err}");
        for (name, code) in [("a", 2), ("b", 3)] {
            let (_, replay) = err
                .split_once(&format!("::group::./{name}.sh — exit {code}\n"))
                .expect(&err);
            let (body, tail) = suspended_body(replay);
            assert_eq!(body, format!("{name} error\n"));
            assert!(tail.starts_with("::endgroup::\n"), "{tail}");
        }
    }
}

#[test]
fn success_replay_is_opt_in_and_empty_successes_have_no_group() {
    for (mode, copies) in [("off", 1), ("plain", 2), ("grouped", 2)] {
        let project = Project::new(&format!("[output.replay]\nsuccess = '{mode}'\n"));
        project.script("ok.sh", "printf 'a warning\\n' >&2; printf 'ok'");
        project.script("empty.sh", "true");
        let output = project
            .command(&["-p", "./ok.sh", "./empty.sh"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ok");
        let err = String::from_utf8(output.stderr).unwrap();
        assert_eq!(err.matches("a warning").count(), copies, "{err}");
        assert_eq!(
            err.matches("::group::").count(),
            usize::from(mode == "grouped"),
            "{err}"
        );
    }
}

#[test]
fn failure_modes_and_empty_stderr_failures() {
    for mode in ["auto", "plain", "grouped", "off"] {
        let project = Project::new(&format!("[output.replay]\nfailure = '{mode}'\n"));
        project.script("empty.sh", "exit 23");
        let output = project.command(&["./empty.sh"]).output().unwrap();
        assert_eq!(output.status.code(), Some(23));
        assert_eq!(output.stdout, b"");
        let err = String::from_utf8(output.stderr).unwrap();
        assert_eq!(err.contains("./empty.sh — exit 23"), mode != "off", "{err}");
        assert_eq!(err.contains("::group::"), mode == "grouped", "{err}");
    }
}

#[test]
fn stderr_is_live_before_the_task_can_exit_even_without_a_newline() {
    let project = Project::new("");
    // The child cannot finish until the test observes its stderr and releases it.
    project.script(
        "gate.sh",
        "printf 'LIVE' >&2; read -r gate; printf 'done'; exit 4",
    );
    let mut child = project
        .command(&["./gate.sh"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut stderr = stderr;
        let mut seen = Vec::new();
        let mut byte = [0];
        while stderr.read(&mut byte).unwrap() != 0 {
            seen.push(byte[0]);
            if seen.ends_with(b"LIVE") {
                let _ = tx.send(());
            }
        }
        seen
    });
    let live = rx.recv_timeout(Duration::from_secs(5));
    // Always release/reap the child, including if the assertion will fail.
    child.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let err = reader.join().unwrap();
    assert!(
        live.is_ok(),
        "stderr was held until exit: {:?}",
        String::from_utf8_lossy(&err)
    );
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(output.stdout, b"done");
}

#[test]
fn replayed_workflow_commands_are_inert_but_live_commands_are_preserved() {
    let payload = concat!(
        "::error file=x::bad\nprefix ##[error]bad\n::group::child\n",
        "::endgroup::\n::stop-commands::token\n::token::\n",
    );
    for mode in ["plain", "grouped"] {
        let project = Project::new(&format!("[output.replay]\nfailure = '{mode}'\n"));
        project.script(
            "commands.sh",
            "printf '%s\\n' '::error file=x::bad' 'prefix ##[error]bad' '::group::child' \
             '::endgroup::' '::stop-commands::token' '::token::' >&2; exit 1",
        );
        let output = project.command(&["./commands.sh"]).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        let err = String::from_utf8(output.stderr).unwrap();
        assert_eq!(err.matches(payload).count(), 2, "{err}");
        let (before_live, after_live) = err.split_once(payload).expect(&err);
        assert!(!before_live.contains("::stop-commands::"), "{err}");
        let start = after_live.find("::stop-commands::").expect(&err);
        let (body, tail) = suspended_body(&after_live[start..]);
        if mode == "grouped" {
            assert!(after_live[..start].ends_with("::group::./commands.sh — exit 1\n"));
            assert_eq!(body, payload);
            assert_eq!(tail, "::endgroup::\n");
        } else {
            assert!(!after_live[..start].contains("::group::"));
            assert_eq!(body, format!("./commands.sh — exit 1\n{payload}"));
            assert_eq!(tail, "");
        }
    }
}

#[test]
fn parallel_stdout_preserves_non_utf8_and_missing_final_newline() {
    let project = Project::new("[output.parallel]\nbuffer = true\n");
    project.script("data.sh", "printf '\\377x'");
    project.script("empty.sh", "true");
    let output = project
        .command(&["-p", "./data.sh", "./empty.sh"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"\xffx");
}

#[test]
fn actual_pipeline_parses_task_json_without_quiet() {
    let project = Project::new("");
    project.script("json.sh", "printf '{\"ok\":true}'");
    let mut source = project
        .command(&["./json.sh"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A separate consumer receives exactly the child's data through a real pipe.
    let mut consumer = Command::new("cat")
        .stdin(source.stdout.take().unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(consumer.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["ok"],
        true
    );
    assert!(source.wait_with_output().unwrap().status.success());
    assert!(consumer.wait().unwrap().success());
}

#[test]
fn nested_runner_is_replayed_once_by_the_outer_task() {
    let project = Project::new("");
    project.script("inner.sh", "printf 'nested error\\n' >&2; exit 8");
    project.script(
        "outer.sh",
        &format!("'{}' run ./inner.sh", env!("CARGO_BIN_EXE_runner")),
    );
    let output = project.command(&["./outer.sh"]).output().unwrap();
    let err = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(8));
    assert_eq!(err.matches("nested error").count(), 2, "{err}");
    assert!(!err.contains("./inner.sh — exit"), "{err}");
    assert!(err.contains("./outer.sh — exit 8"), "{err}");
}

#[test]
fn non_actions_and_explicit_opt_out_do_not_replay() {
    for config in [
        "",
        "[output]\ngroups = false\n",
        "[output.replay]\nfailure = 'off'\n",
    ] {
        let project = Project::new(config);
        project.script("fail.sh", "printf 'only once\\n' >&2; exit 1");
        let mut command = project.command(&["./fail.sh"]);
        if config.is_empty() {
            command.env_remove("GITHUB_ACTIONS");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let err = String::from_utf8(output.stderr).unwrap();
        assert_eq!(err.matches("only once").count(), 1, "{err}");
        assert!(!err.contains("::group::"), "{err}");
    }
}

#[test]
fn one_failure_among_several_tasks_is_plain_and_signals_keep_their_exit_code() {
    let project = Project::new("");
    project.script("ok.sh", "printf 'success warning\\n' >&2");
    project.script("signal.sh", "kill -TERM $$");
    let output = project
        .command(&["-pk", "./ok.sh", "./signal.sh"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(143));
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("./signal.sh — exit 143"), "{err}");
    assert!(!err.contains("::group::"), "{err}");
    assert_eq!(err.matches("success warning").count(), 1, "{err}");
}

#[test]
fn task_stream_suppression_applies_to_live_output_and_replay() {
    let project = Project::new("[tasks.'./hidden.sh'.output.task]\nstderr = false\n");
    project.script("hidden.sh", "printf 'hidden data\\n' >&2; exit 5");
    for args in [vec!["./hidden.sh"], vec!["-p", "./hidden.sh", "./ok.sh"]] {
        project.script("ok.sh", "true");
        let output = project.command(&args).output().unwrap();
        let err = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(5));
        assert!(!err.contains("hidden data"), "{err}");
        assert!(err.contains("./hidden.sh — exit 5"), "{err}");
    }
}

#[test]
fn quiet_suppresses_replay_but_preserves_live_stderr() {
    let project = Project::new("");
    project.script("fail.sh", "printf 'live failure\\n' >&2; exit 6");
    let output = project.command(&["-q", "./fail.sh"]).output().unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert_eq!(output.stderr, b"live failure\n");
}

#[test]
fn kill_on_fail_does_not_count_cancelled_siblings_as_failures() {
    let project = Project::new("");
    project.script("fail.sh", "exit 9");
    project.script("slow.sh", "exec sleep 30");
    let output = project
        .command(&["-pK", "./fail.sh", "./slow.sh"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(9));
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("./fail.sh — exit 9"), "{err}");
    assert!(!err.contains("./slow.sh — exit"), "{err}");
    assert!(!err.contains("::group::"), "{err}");
}

#[test]
fn a_group_starts_on_a_new_line_after_unterminated_live_stderr() {
    let project = Project::new("[output.replay]\nfailure = 'grouped'\n");
    project.script("fail.sh", "printf 'partial' >&2; exit 1");
    let output = project.command(&["./fail.sh"]).output().unwrap();
    let err = String::from_utf8(output.stderr).unwrap();
    let (_, replay) = err
        .split_once("partial\n::group::./fail.sh — exit 1\n")
        .expect(&err);
    let (body, tail) = suspended_body(replay);
    assert_eq!(body, "partial\n");
    assert_eq!(tail, "::endgroup::\n");
}

#[test]
fn install_failure_replays_stderr_without_decorating_stdout() {
    let project = Project::new("");
    std::fs::write(
        project.0.path().join("package.json"),
        r#"{"name":"fixture","packageManager":"npm@11.0.0"}"#,
    )
    .unwrap();
    project.script(
        "npm",
        "printf 'install-data'; printf 'install failed\\n' >&2; exit 12",
    );
    let paths = std::iter::once(project.0.path().to_path_buf())
        .chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ))
        .collect::<Vec<_>>();
    let output = support::command(env!("CARGO_BIN_EXE_runner"))
        .arg("--dir")
        .arg(project.0.path())
        .args(["install", "--no-tools"])
        .env("GITHUB_ACTIONS", "true")
        .env("PATH", std::env::join_paths(paths).unwrap())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(12),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"install-data");
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(err.contains("install npm — exit 12"), "{err}");
    assert_eq!(err.matches("install failed").count(), 2, "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn parallel_install_replay_uses_the_exit_code_after_output_delivery() {
    for child_code in [0, 12] {
        let project = Project::new("");
        std::fs::write(
            project.0.path().join("package.json"),
            r#"{"name":"fixture","packageManager":"npm@11.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            project.0.path().join("Cargo.toml"),
            "[package]\nname = 'fixture'\nversion = '0.0.0'\nedition = '2021'\n",
        )
        .unwrap();
        std::fs::create_dir(project.0.path().join("src")).unwrap();
        std::fs::write(project.0.path().join("src/lib.rs"), "").unwrap();
        project.script("cargo", "exit 0");
        project.script(
            "npm",
            &format!("printf 'data'; printf 'installer message\\n' >&2; exit {child_code}"),
        );
        let paths = std::iter::once(project.0.path().to_path_buf())
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        let output = support::command(env!("CARGO_BIN_EXE_runner"))
            .arg("--dir")
            .arg(project.0.path())
            .args(["install", "--no-tools"])
            .env("GITHUB_ACTIONS", "true")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .stdout(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/dev/full")
                    .unwrap(),
            )
            .output()
            .unwrap();
        let expected = if child_code == 0 { 1 } else { child_code };
        let err = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(expected), "{err}");
        assert!(
            err.contains(&format!("install npm — exit {expected}")),
            "{err}"
        );
        assert_eq!(err.matches("installer message").count(), 2, "{err}");
        assert!(!err.contains("::group::"), "only npm failed: {err}");
    }
}
