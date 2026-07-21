//! Integration coverage for `--quiet` / `RUNNER_QUIET`.
//!
//! The dispatch arrow (`→ <source> <task>`) and the `--explain` resolution
//! trace must stay off stderr when quiet is on. Tests dispatch real tasks in
//! throwaway temp projects so they are deterministic and succeed regardless of
//! which package managers happen to be installed:
//!
//! - the arrow tests use a `Makefile` recipe that runs `true` (`make` is
//!   ubiquitous on dev/CI machines);
//! - the explain test uses a `package.json` script pinned to npm via an empty
//!   lockfile (npm ships with Node on every runner), because `--explain` only
//!   traces package-manager resolution; a `make` task never emits it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Self-cleaning temp directory. Avoids a dev-dependency for the integration
/// crate; the in-crate `test_support::TempDir` is `pub(crate)` and thus not
/// reachable from `tests/`.
struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("runner-quiet-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp project dir");
        Self { path }
    }

    fn file(self, name: &str, contents: &str) -> Self {
        std::fs::write(self.path.join(name), contents).expect("write project file");
        self
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn run_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_run"))
}

fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run the `run` binary against `dir` with every `RUNNER_*` var scrubbed, then
/// `extra_env` applied. Globals (`--dir`, `--quiet`, `--explain`) must precede
/// the task positional, since `trailing_var_arg` consumes everything after it.
fn run_in(dir: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(run_binary());
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RUNNER_")
        {
            cmd.env_remove(&key);
        }
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("run should execute")
}

/// Temp project whose `greet` make recipe just runs `true`.
fn make_project(tag: &str) -> TempProject {
    TempProject::new(tag).file("Makefile", "greet:\n\t@true\n")
}

/// Temp project with a `package.json` `greet` script pinned to npm via an empty
/// lockfile, so resolution is deterministic and emits an explain trace.
fn npm_project(tag: &str) -> TempProject {
    TempProject::new(tag)
        .file("package.json", "{ \"scripts\": { \"greet\": \"true\" } }\n")
        .file("package-lock.json", "{}\n")
}

#[test]
fn quiet_flag_suppresses_dispatch_arrow() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = make_project("flag");
    let output = run_in(proj.path(), &[], &["--quiet", "greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "run --quiet greet should succeed. status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        !stderr.contains('→'),
        "dispatch arrow must be suppressed with --quiet. stderr: {stderr}",
    );
}

#[test]
fn runner_quiet_env_suppresses_dispatch_arrow() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = make_project("env");
    let output = run_in(proj.path(), &[("RUNNER_QUIET", "1")], &["greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "run with RUNNER_QUIET=1 should succeed. status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        !stderr.contains('→'),
        "dispatch arrow must be suppressed with RUNNER_QUIET=1. stderr: {stderr}",
    );
}

#[test]
fn dispatch_arrow_prints_without_quiet() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = make_project("plain");
    let output = run_in(proj.path(), &[], &["greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "run greet should succeed. status: {:?}, stderr: {stderr}",
        output.status,
    );
    assert!(
        stderr.contains('→'),
        "dispatch arrow expected without --quiet. stderr: {stderr}",
    );
}

#[test]
fn quiet_keeps_github_actions_group_markers_off_stdout() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    // Positive control: under Actions the group markers are the whole point,
    // so they must be there without `--quiet`.
    let shown_proj = make_project("gha-on");
    let shown = run_in(shown_proj.path(), &[("GITHUB_ACTIONS", "true")], &["greet"]);
    let shown_out = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown_out.contains("::group::runner: greet") && shown_out.contains("::endgroup::"),
        "expected a group to suppress. stdout: {shown_out}",
    );

    // #86: a parent parsing this stdout (`npm pack --json` piped into a
    // script) got `::group::` in front of the JSON and failed to parse it.
    let proj = make_project("gha-quiet");
    let output = run_in(proj.path(), &[("GITHUB_ACTIONS", "true")], &["-q", "greet"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "run -q greet should succeed. status: {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !stdout.contains("::group::") && !stdout.contains("::endgroup::"),
        "--quiet must leave stdout to the task. stdout: {stdout}",
    );
}

#[test]
fn quiet_suppresses_explain_trace() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    // Positive control: `--explain` alone emits the resolution trace.
    let shown_proj = npm_project("explain-on");
    let shown = run_in(shown_proj.path(), &[], &["--explain", "greet"]);
    let shown_err = String::from_utf8_lossy(&shown.stderr);
    assert!(
        shown.status.success(),
        "run --explain greet should succeed. status: {:?}, stderr: {shown_err}",
        shown.status,
    );
    assert!(
        shown_err.contains("resolved:"),
        "--explain should emit a resolution trace to suppress. stderr: {shown_err}",
    );

    // `--quiet` suppresses both the arrow and that trace.
    let hidden_proj = npm_project("explain-off");
    let hidden = run_in(hidden_proj.path(), &[], &["--quiet", "--explain", "greet"]);
    let hidden_err = String::from_utf8_lossy(&hidden.stderr);
    assert!(
        hidden.status.success(),
        "run --quiet --explain greet should succeed. status: {:?}, stderr: {hidden_err}",
        hidden.status,
    );
    assert!(
        !hidden_err.contains('→') && !hidden_err.contains("resolved:"),
        "--quiet must suppress the arrow and the explain trace. stderr: {hidden_err}",
    );
}

/// #93: `--quiet` now crosses into the host tool. On an npm project npm's
/// lifecycle banner (`> greet` / `> <cmd>`) is on **stdout**; `-q` must pass
/// `npm --silent` so that banner is gone and stdout carries only the task's
/// own output, the machine-readable pipeline the issue asked for.
#[test]
fn quiet_silences_npm_host_banner_on_stdout() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let proj = TempProject::new("npm-host-banner")
        .file(
            "package.json",
            "{ \"scripts\": { \"greet\": \"echo SENTINEL-OUT\" } }\n",
        )
        .file("package-lock.json", "{}\n");

    // Positive control: without -q npm prints its banner to stdout.
    let loud = run_in(proj.path(), &[], &["greet"]);
    let loud_out = String::from_utf8_lossy(&loud.stdout);
    assert!(
        loud_out.contains("> greet"),
        "npm banner expected on stdout without -q. stdout: {loud_out}",
    );

    let quiet = run_in(proj.path(), &[], &["-q", "greet"]);
    let quiet_out = String::from_utf8_lossy(&quiet.stdout);
    assert!(
        quiet.status.success(),
        "run -q greet should succeed. stderr: {}",
        String::from_utf8_lossy(&quiet.stderr),
    );
    assert!(
        quiet_out.contains("SENTINEL-OUT"),
        "the task's own output must survive -q. stdout: {quiet_out}",
    );
    assert!(
        !quiet_out.contains("> greet"),
        "-q must silence npm's host banner on stdout (#93). stdout: {quiet_out}",
    );
}

/// #93 acceptance: a script that calls `runner` again inherits the quiet level
/// via the exported `RUNNER_QUIET`, so the nested npm banner is silenced too.
#[test]
fn quiet_propagates_to_nested_runner() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let nested = format!("{} inner", run_binary().display());
    let scripts = format!(
        "{{ \"scripts\": {{ \"build\": \"{}\", \"inner\": \"echo INNER-OUT\" }} }}\n",
        nested.replace('\\', "\\\\"),
    );
    let proj = TempProject::new("npm-nested-quiet")
        .file("package.json", &scripts)
        .file("package-lock.json", "{}\n");

    let output = run_in(proj.path(), &[], &["-q", "build"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "run -q build should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains("INNER-OUT"),
        "the nested task's output must survive. stdout: {stdout}",
    );
    assert!(
        !stdout.contains("> build") && !stdout.contains("> inner"),
        "both the outer and inherited-quiet inner npm banners must be gone. stdout: {stdout}",
    );
}
