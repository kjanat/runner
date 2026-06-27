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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{io::Read, thread};

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
fn sequential_chain_emits_per_task_timing_on_stderr() {
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
        ])
        // Scrub GITHUB_ACTIONS so the timing line shape is deterministic
        // regardless of the host CI environment.
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success.\nstdout: {stdout}\nstderr: {stderr}",
    );
    // One timing line per task, on stderr (sequential meta-output lives on
    // stderr like the dispatch arrow). Assert presence/count, never values.
    assert_eq!(
        stderr.matches("finished in").count(),
        2,
        "expected one timing line per sequential task. stderr: {stderr}",
    );
    assert!(
        stderr.contains("(exit 0)"),
        "expected exit code in timing line. stderr: {stderr}",
    );
}

#[test]
fn streaming_parallel_chain_emits_per_task_timing_on_stderr() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // `chain-parallel-fail`'s runner.toml forces the live (streaming) muxer on
    // both the CI and non-CI paths, so this deterministically exercises the
    // streaming timing emission. fail-mid exits 7; the default FailFast policy
    // lets the already-spawned siblings finish, so all three report timing.
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

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(7),
        "expected exit 7 from fail-mid.\nstdout: {stdout}\nstderr: {stderr}",
    );
    assert_eq!(
        stderr.matches("finished in").count(),
        3,
        "expected one timing line per streaming task. stderr: {stderr}",
    );
    assert!(
        stderr.contains("(exit 7)"),
        "fail-mid's non-zero exit should surface in its timing line. stderr: {stderr}",
    );
}

#[test]
fn grouped_parallel_chain_folds_timing_into_block_footer() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // `parallel-grouped`'s runner.toml opts into grouped output outside GitHub
    // Actions, so each task's duration is folded into its block footer on
    // stdout (not a stderr meta-line).
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("parallel-grouped").to_str().unwrap(),
            "run",
            "-p",
            "build",
            "test",
        ])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert_eq!(
        stdout.matches("finished in").count(),
        2,
        "expected one footer per grouped task on stdout. stdout: {stdout}",
    );
    assert!(
        stdout.contains("(exit 0)"),
        "grouped footer should carry the exit code. stdout: {stdout}",
    );
}

#[test]
fn quiet_suppresses_chain_timing() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // `RUNNER_QUIET` mutes diagnostic meta-output (dispatch arrow, timing).
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "-s",
            "build",
            "test",
        ])
        .env("RUNNER_QUIET", "1")
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success.\nstdout: {stdout}\nstderr: {stderr}",
    );
    assert_eq!(
        stderr.matches("finished in").count(),
        0,
        "--quiet must suppress timing lines. stderr: {stderr}",
    );
    // The tasks still ran — quiet hides meta-output, not task output.
    assert!(
        stdout.contains("build-ran") && stdout.contains("test-ran"),
        "tasks should still run under --quiet. stdout: {stdout}",
    );
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
    // The fixture's runner.toml disables parallel grouping on both the CI
    // (`[github].group_parallel`) and non-CI (`[parallel].grouped`) paths, so
    // this deterministically exercises the live line-prefixed muxer
    // regardless of environment. Output is line-prefixed.
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

#[test]
fn install_completion_includes_tasks_and_options() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }

    let output = Command::new(runner_binary())
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "4")
        .args([
            "--",
            "runner",
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "install",
            "",
        ])
        .output()
        .expect("runner binary spawns");

    assert!(
        output.status.success(),
        "completion should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("just\x1fbuild"),
        "install completion should include task candidates. stdout: {stdout}",
    );
    assert!(
        stdout.contains("Options\x1f--frozen"),
        "install completion should keep option candidates. stdout: {stdout}",
    );
}

#[test]
fn chain_prevalidates_all_tokens_before_running_any_task() {
    // A chain with a clearly-broken third token (`lint:cargo` — the
    // reversed qualifier we error on) must NOT run `build` and `test`
    // to completion first. The pre-validation in `run_chain` should
    // bail before any sibling dispatches.
    //
    // No `just_available()` gate — `precheck_task` works off
    // `ctx.tasks` (populated from the justfile by the detector) and
    // never spawns the just binary itself. If `just` is missing the
    // tasks table is empty, which would make this test pass for the
    // wrong reason (qualified miss on `build`), so we still assert
    // on the `cargo:lint` hint specifically.
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
            "lint:cargo",
        ])
        .output()
        .expect("runner binary spawns");

    assert!(
        !output.status.success(),
        "chain with reversed-qualifier item must fail",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cargo:lint"),
        "expected `did you mean cargo:lint?` hint in stderr: {stderr}",
    );
    // The fixture's `build` and `test` recipes echo `build-ran` /
    // `test-ran` (see `tests/fixtures/chain-sequential/justfile`).
    // Their absence proves the pre-validation fired *before* any
    // sibling dispatch.
    assert!(
        !stdout.contains("build-ran"),
        "pre-validation should have skipped `build`. stdout: {stdout}",
    );
    assert!(
        !stdout.contains("test-ran"),
        "pre-validation should have skipped `test`. stdout: {stdout}",
    );
}

#[test]
fn sequential_chain_wraps_steps_in_github_actions_groups() {
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
        ])
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );

    let g_build = stdout
        .find("::group::runner: build")
        .unwrap_or_else(|| panic!("missing build group. stdout: {stdout}"));
    let end_build = g_build
        + stdout[g_build..]
            .find("::endgroup::")
            .unwrap_or_else(|| panic!("build group not closed. stdout: {stdout}"));
    let g_test = stdout
        .find("::group::runner: test")
        .unwrap_or_else(|| panic!("missing test group. stdout: {stdout}"));
    let build_ran = stdout
        .find("build-ran")
        .unwrap_or_else(|| panic!("build-ran missing. stdout: {stdout}"));

    // build's group opens, contains its output, and closes before test's
    // group opens — flat, non-overlapping groups (GitHub Actions can't
    // render nested ones).
    assert!(
        g_build < build_ran && build_ran < end_build && end_build < g_test,
        "expected build group to open, contain build-ran, close, then test group. stdout: {stdout}",
    );
    assert_eq!(
        stdout.matches("::group::runner: ").count(),
        2,
        "expected exactly two groups. stdout: {stdout}",
    );
    assert_eq!(
        stdout.matches("::endgroup::").count(),
        2,
        "expected exactly two endgroups. stdout: {stdout}",
    );
}

#[test]
fn single_task_is_grouped_under_github_actions() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // A bare single task (no `-s`) still gets one group under GitHub Actions
    // — grouping covers every task run, not just multi-step chains.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "build",
        ])
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert!(
        stdout.contains("::group::runner: build"),
        "single task should be wrapped in a group. stdout: {stdout}",
    );
    assert_eq!(
        stdout.matches("::group::").count(),
        1,
        "exactly one group for a single task. stdout: {stdout}",
    );
}

#[test]
fn no_groups_emitted_outside_github_actions() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Scrub GITHUB_ACTIONS so this is deterministic even when the test host
    // itself runs under GitHub Actions (mirrors info_deprecation.rs).
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "-s",
            "build",
            "test",
        ])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert!(
        !stdout.contains("::group::"),
        "no GHA groups in a normal terminal. stdout: {stdout}",
    );
    assert!(
        !stdout.contains("::endgroup::"),
        "no GHA endgroups in a normal terminal. stdout: {stdout}",
    );
}

#[test]
fn config_opt_out_disables_grouping_under_github_actions() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // The `github-no-group` fixture ships a runner.toml with
    // `[github] group_output = false`, so even under GitHub Actions no
    // groups are emitted.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("github-no-group").to_str().unwrap(),
            "run",
            "build",
        ])
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert!(
        !stdout.contains("::group::"),
        "config opt-out must suppress groups. stdout: {stdout}",
    );
}

#[test]
fn github_group_output_false_restores_live_parallel_muxer() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("github-no-group").to_str().unwrap(),
            "run",
            "-p",
            "build",
            "test",
        ])
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert!(
        stdout.contains("[build") && stdout.contains("[test"),
        "GHA group_output=false should restore live prefixes. stdout: {stdout}",
    );
    assert!(
        !stdout.contains("::group::") && !stdout.contains("runner: build"),
        "GHA group_output=false should not emit grouped blocks. stdout: {stdout}",
    );
}

#[test]
fn parallel_chain_grouped_under_github_actions() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Default `[github].group_parallel` buffers each task and emits it as its
    // own ::group:: block under GitHub Actions — no live `[task]` prefixes.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("chain-sequential").to_str().unwrap(),
            "run",
            "-p",
            "build",
            "test",
        ])
        .env("GITHUB_ACTIONS", "true")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert!(
        !stdout.contains("[build"),
        "grouped parallel output must not use live prefixes. stdout: {stdout}",
    );

    // Each task's output sits inside its own group; completion order between
    // the two is nondeterministic, so assert per-task containment, not order.
    let g_build = stdout
        .find("::group::runner: build")
        .unwrap_or_else(|| panic!("missing build group. stdout: {stdout}"));
    let build_ran = stdout
        .find("build-ran")
        .unwrap_or_else(|| panic!("build-ran missing. stdout: {stdout}"));
    assert!(
        g_build < build_ran,
        "build output must sit inside build's group. stdout: {stdout}",
    );
    assert!(
        stdout.contains("::group::runner: test"),
        "missing test group. stdout: {stdout}",
    );
    assert_eq!(
        stdout.matches("::group::runner: ").count(),
        2,
        "expected exactly two groups. stdout: {stdout}",
    );
    assert_eq!(
        stdout.matches("::endgroup::").count(),
        2,
        "expected exactly two endgroups. stdout: {stdout}",
    );
}

#[test]
fn parallel_chain_grouped_with_plain_headers_outside_github_actions() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Grouping is not GitHub-specific: outside Actions each block gets a plain
    // `runner: <task>` header and no ::group:: workflow-command bloat.
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("parallel-grouped").to_str().unwrap(),
            "run",
            "-p",
            "build",
            "test",
        ])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}"
    );
    assert!(
        !stdout.contains("::group::") && !stdout.contains("::endgroup::"),
        "no workflow-command syntax outside GitHub Actions. stdout: {stdout}",
    );
    assert!(
        !stdout.contains("[build"),
        "grouped parallel output must not use live prefixes. stdout: {stdout}",
    );

    // Each task gets a plain header block, with its output underneath.
    let h_build = stdout
        .find("runner: build")
        .unwrap_or_else(|| panic!("missing build header. stdout: {stdout}"));
    let build_ran = stdout
        .find("build-ran")
        .unwrap_or_else(|| panic!("build-ran missing. stdout: {stdout}"));
    assert!(
        h_build < build_ran,
        "build output must follow its header. stdout: {stdout}",
    );
    assert!(
        stdout.contains("runner: test"),
        "missing test header. stdout: {stdout}",
    );
}

#[test]
fn parallel_grouped_preserves_child_stderr_stream() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("parallel-grouped").to_str().unwrap(),
            "run",
            "-p",
            "build",
            "err",
        ])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected success. stdout: {stdout}; stderr: {stderr}"
    );
    assert!(
        stderr.contains("err-ran"),
        "child stderr must stay on stderr. stderr: {stderr}",
    );
    assert!(
        !stdout.contains("err-ran"),
        "child stderr must not be replayed to stdout. stdout: {stdout}",
    );
}

#[test]
fn parallel_grouped_does_not_wait_forever_on_inherited_stdout() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let mut child = Command::new(runner_binary())
        .args([
            "--dir",
            fixture("parallel-grouped").to_str().unwrap(),
            "run",
            "-p",
            "hold-open",
            "build",
        ])
        .env_remove("GITHUB_ACTIONS")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runner binary spawns");

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("runner status checks") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("grouped parallel waited on inherited stdout for too long");
        }
        thread::sleep(Duration::from_millis(50));
    };

    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout)
            .expect("stdout should be readable");
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr)
            .expect("stderr should be readable");
    }

    assert!(status.success(), "stdout: {stdout}; stderr: {stderr}");
    assert!(
        stdout.contains("foreground-ran"),
        "completed task output should still flush. stdout: {stdout}",
    );
}
