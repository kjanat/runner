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

/// Build an isolated temp project *outside* the repo tree (so detection can't
/// ascend into the parent crate and pull in its package manager) where Cargo
/// is the sole detected PM. `runner install` then resolves to an offline
/// `cargo fetch` for this zero-dependency crate, plus a `just` task to fan out
/// after install. The caller removes the returned dir when done.
fn isolated_install_project() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("runner-install-it-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).expect("create temp project dir");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"runner-install-it-fixture\"\nversion = \"0.0.0\"\nedition = \
         \"2021\"\npublish = false\n\n[dependencies]\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(dir.join("src/lib.rs"), "").expect("write lib.rs");
    std::fs::write(dir.join("justfile"), "build:\n\t@echo build-ran\n").expect("write justfile");
    dir
}

#[test]
fn sequential_chain_runs_in_order() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let output = Command::new(runner_binary())
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build", "test", "lint"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args([
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
        .arg("--dir")
        .arg(fixture("chain-parallel-fail"))
        .args(["run", "-p", "ok-one", "fail-mid", "ok-two"])
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

#[cfg(unix)]
#[test]
fn streaming_parallel_chain_returns_despite_stdio_holding_descendant() {
    // `daemon` backgrounds a 30s sleeper that inherits the piped stdout
    // and exits immediately. Pipe readers only see EOF once every write
    // end closes, so an unbounded reader join would block on the sleeper
    // for the full 30s after both direct children are reaped. The
    // streaming supervisor must instead drain with the bounded grace and
    // return promptly. Unix-only: the recipe needs `sh`.
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let started = Instant::now();
    let output = Command::new(runner_binary())
        .arg("--dir")
        .arg(fixture("chain-parallel-fail"))
        .args(["run", "-p", "ok-one", "daemon"])
        .output()
        .expect("runner binary spawns");
    let elapsed = started.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "both tasks exit 0.\nstdout: {stdout}\nstderr: {stderr}",
    );
    // Generous CI margin: anything under the sleeper's 30s proves the
    // supervisor didn't wait for the abandoned reader; a healthy run is
    // ~0.5s (spawn + 500ms drain grace).
    assert!(
        elapsed < Duration::from_secs(15),
        "streaming `-p` must not block on a descendant holding stdio; took {elapsed:?}",
    );
}

#[cfg(unix)]
#[test]
fn streaming_parallel_spawn_failure_returns_despite_stdio_holding_descendant() {
    // Spawn-phase sibling failure variant of the drain guard: `daemon`
    // spawns and leaves its 30s sleeper holding the pipe, then the second
    // token fails to spawn (no such binary, PM-exec direct spawn ENOENT).
    // The error cleanup must drain readers with the bounded grace, not
    // block on the sleeper.
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let started = Instant::now();
    let output = Command::new(runner_binary())
        .arg("--dir")
        .arg(fixture("chain-parallel-fail"))
        .args(["run", "-p", "daemon", "definitely-not-a-binary-xyz"])
        .output()
        .expect("runner binary spawns");
    let elapsed = started.elapsed();

    assert!(
        !output.status.success(),
        "second token cannot spawn; the chain must fail",
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "spawn-failure cleanup must not block on a descendant holding stdio; took {elapsed:?}",
    );
}

#[test]
fn parallel_install_chain_times_the_install_step() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Parity guard for install-chain timing: the parallel (`-p`) install path
    // runs install imperatively before fanning the tasks out, so it must wrap
    // that install in the same timing the sequential path's synthetic install
    // head gets — otherwise `runner install -p ...` would time the tasks but
    // not the install step, while `-s` times both.
    let project = isolated_install_project();
    let output = Command::new(runner_binary())
        .arg("--dir")
        .arg(&project)
        .args(["install", "-p", "build"])
        .env_remove("GITHUB_ACTIONS")
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Remove the temp project up front so a failing assertion can't leak it.
    let _ = std::fs::remove_dir_all(&project);

    assert!(
        output.status.success(),
        "expected success.\nstdout: {stdout}\nstderr: {stderr}",
    );
    // Only the install timing line carries both "install" and "finished in":
    // "installing with cargo" lacks the latter, and the fanned-out task's line
    // names "build". Its presence is exactly what the imperative path dropped.
    assert!(
        stderr
            .lines()
            .any(|line| line.contains("install") && line.contains("finished in")),
        "parallel install step must emit its own timing line. stderr: {stderr}",
    );
    // The post-install task still runs and gets its own timing line, so both
    // the install head and the task are accounted for.
    assert!(
        stdout.contains("build-ran"),
        "post-install task should run. stdout: {stdout}",
    );
    assert!(
        stderr.matches("finished in").count() >= 2,
        "expected timing for both the install step and the task. stderr: {stderr}",
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
        .arg("--dir")
        .arg(fixture("parallel-grouped"))
        .args(["run", "-p", "build", "test"])
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
fn grouped_parallel_chain_folds_timing_into_group_footer_under_actions() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // Companion to `grouped_parallel_chain_folds_timing_into_block_footer`:
    // under GitHub Actions the same grouped fixture wraps each task in a
    // `::group::runner: <task>` block and folds its duration into the block's
    // footer *inside* the group, before `::endgroup::` (see `flush_task_group`'s
    // `gha_syntax` path in src/chain/exec.rs — the footer is written before the
    // GroupGuard's Drop emits `::endgroup::`).
    let output = Command::new(runner_binary())
        .arg("--dir")
        .arg(fixture("parallel-grouped"))
        .args(["run", "-p", "build", "test"])
        .env("GITHUB_ACTIONS", "true")
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
        "expected one folded footer per grouped task on stdout. stdout: {stdout}",
    );
    assert!(
        stdout.contains("(exit 0)"),
        "grouped GHA footer should carry the exit code. stdout: {stdout}",
    );

    // The footer must live *inside* each task's `::group::` block, before its
    // `::endgroup::`. Groups are flat (GitHub Actions can't nest them) and the
    // supervisor flushes one block at a time, so the markers strictly alternate
    // group/endgroup; pair them up and assert each block carries its timing.
    let groups: Vec<usize> = stdout
        .match_indices("::group::runner: ")
        .map(|(i, _)| i)
        .collect();
    let ends: Vec<usize> = stdout
        .match_indices("::endgroup::")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        groups.len(),
        2,
        "expected exactly two groups. stdout: {stdout}"
    );
    assert_eq!(
        ends.len(),
        2,
        "expected exactly two endgroups. stdout: {stdout}",
    );
    for (start, end) in groups.iter().zip(ends.iter()) {
        assert!(
            start < end,
            "each group must open before it closes. stdout: {stdout}",
        );
        let block = &stdout[*start..*end];
        assert!(
            block.contains("finished in") && block.contains("(exit 0)"),
            "timing footer must sit inside the group block, before ::endgroup::. block: {block}",
        );
    }
}

#[test]
fn quiet_suppresses_chain_timing() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    // `RUNNER_QUIET` mutes diagnostic meta-output (dispatch arrow, timing).
    let output = Command::new(runner_binary())
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build", "test"])
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
        .arg("--dir")
        .arg(fixture("chain-parallel-fail"))
        .args(["run", "-p", "ok-one", "fail-mid", "ok-two"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "-p", "build"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build --release"])
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
        .args(["--", "runner", "--dir"])
        .arg(fixture("chain-sequential"))
        .args(["install", ""])
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
fn chain_mode_completion_offers_tasks_after_first_task() {
    if !just_available() {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }

    // `run -s build <TAB>` — the trailing words are extra task names in
    // chain mode, so the second position must offer task candidates.
    let output = Command::new(runner_binary())
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "6")
        .args(["--", "runner", "--dir"])
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build", ""])
        .output()
        .expect("runner binary spawns");

    assert!(
        output.status.success(),
        "completion should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("just\x1ftest"),
        "chain-mode completion should offer task candidates. stdout: {stdout}",
    );

    // Without a chain flag the trailing words are the task's own args —
    // task names would be noise there.
    let output = Command::new(runner_binary())
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "5")
        .args(["--", "runner", "--dir"])
        .arg(fixture("chain-sequential"))
        .args(["run", "build", ""])
        .output()
        .expect("runner binary spawns");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("just\x1ftest"),
        "non-chain trailing position must not offer task candidates. stdout: {stdout}",
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build", "test", "lint:cargo"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build", "test"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "build"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-s", "build", "test"])
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
        .arg("--dir")
        .arg(fixture("github-no-group"))
        .args(["run", "build"])
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
        .arg("--dir")
        .arg(fixture("github-no-group"))
        .args(["run", "-p", "build", "test"])
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
        .arg("--dir")
        .arg(fixture("chain-sequential"))
        .args(["run", "-p", "build", "test"])
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
        .arg("--dir")
        .arg(fixture("parallel-grouped"))
        .args(["run", "-p", "build", "test"])
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
        .arg("--dir")
        .arg(fixture("parallel-grouped"))
        .args(["run", "-p", "build", "err"])
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
        .arg("--dir")
        .arg(fixture("parallel-grouped"))
        .args(["run", "-p", "hold-open", "build"])
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
