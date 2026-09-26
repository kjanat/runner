//! Integration coverage for `--quiet` / `RUNNER_QUIET`.
//!
//! The dispatch arrow (`→ <source> <task>`) and the `--dry-run` resolution
//! trace must stay off stderr when quiet is on. Tests dispatch real tasks in
//! throwaway temp projects so they are deterministic and succeed regardless of
//! which package managers happen to be installed:
//!
//! - the arrow tests use a `Makefile` recipe that runs `true` (`make` is
//!   ubiquitous on dev/CI machines);
//! - the explain test uses a `package.json` script pinned to npm via an empty
//!   lockfile (npm ships with Node on every runner), because `--dry-run` only
//!   traces package-manager resolution; a `make` task never emits it.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use actions_rs::env::vars::GITHUB_ACTIONS;

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

    fn dir(self, name: &str) -> Self {
        std::fs::create_dir_all(self.path.join(name)).expect("create project subdirectory");
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

fn group(title: &str) -> String {
    actions_rs::WorkflowCommand::new("group")
        .message(title)
        .to_string()
}

fn endgroup() -> String {
    actions_rs::WorkflowCommand::new("endgroup").to_string()
}

fn run_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_run"))
}

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run the `run` binary against `dir` with every `RUNNER_*` var scrubbed, then
/// `extra_env` applied. Globals (`--dir`, `--quiet`, `--dry-run`) must precede
/// the task positional, since `trailing_var_arg` consumes everything after it.
fn run_in(dir: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Output {
    command_in(run_binary(), dir, extra_env, args)
}

fn runner_in(dir: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Output {
    command_in(runner_binary(), dir, extra_env, args)
}

fn command_in(binary: PathBuf, dir: &Path, extra_env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = support::command(binary);
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
    let shown = run_in(shown_proj.path(), &[(GITHUB_ACTIONS, "true")], &["greet"]);
    let shown_out = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown_out.contains(&group("runner: greet")) && shown_out.contains(&endgroup()),
        "expected a group to suppress. stdout: {shown_out}",
    );

    // #86: a parent parsing this stdout (`npm pack --json` piped into a
    // script) got `::group::` in front of the JSON and failed to parse it.
    let proj = make_project("gha-quiet");
    let output = run_in(proj.path(), &[(GITHUB_ACTIONS, "true")], &["-q", "greet"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "run -q greet should succeed. status: {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !stdout.contains(&group("")) && !stdout.contains(&endgroup()),
        "--quiet must leave stdout to the task. stdout: {stdout}",
    );
}

#[test]
fn explain_overrides_quiet_and_reports_effective_policy() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    // Positive control: `--dry-run` alone emits the resolution trace.
    let shown_proj = npm_project("explain-on");
    let shown = run_in(shown_proj.path(), &[], &["--dry-run", "greet"]);
    let shown_err = String::from_utf8_lossy(&shown.stderr);
    assert!(
        shown.status.success(),
        "run --dry-run greet should succeed. status: {:?}, stderr: {shown_err}",
        shown.status,
    );
    assert!(
        shown_err.contains("resolved:"),
        "--dry-run should emit a resolution trace to suppress. stderr: {shown_err}",
    );

    // Explicit explain remains visible and reports effective policy.
    let hidden_proj = npm_project("explain-off");
    let hidden = run_in(hidden_proj.path(), &[], &["--quiet", "--dry-run", "greet"]);
    let hidden_err = String::from_utf8_lossy(&hidden.stderr);
    assert!(
        hidden.status.success(),
        "run --quiet --dry-run greet should succeed. status: {:?}, stderr: {hidden_err}",
        hidden.status,
    );
    assert!(
        !hidden_err.contains('→')
            && hidden_err.contains("resolved:")
            && hidden_err.contains("level=quiet")
            && hidden_err.contains("diagnostics=normal"),
        "--dry-run must override quiet presentation. stderr: {hidden_err}",
    );
}

/// npm 11 prints its lifecycle
/// banner (`> greet` / `> <cmd>`) on stdout. npm 12's `@npmcli/run-script` 11
/// emits the same information as `npm notice run` logs on stderr. `-q` must
/// suppress either form so only the task's own output remains.
#[test]
fn host_quiet_starts_at_second_level() {
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

    // Positive control: npm 11 writes the banner to stdout; npm 12 writes it
    // as notice logs to stderr.
    let loud = run_in(
        proj.path(),
        &[("NPM_CONFIG_LOGLEVEL", "notice")],
        &["greet"],
    );
    let loud_out = String::from_utf8_lossy(&loud.stdout);
    let loud_err = String::from_utf8_lossy(&loud.stderr);
    assert!(
        loud.status.success(),
        "run greet should succeed. stderr: {}",
        String::from_utf8_lossy(&loud.stderr),
    );
    assert!(
        loud_out.lines().any(|line| line == "SENTINEL-OUT"),
        "the task's own output must survive without -q. stdout: {loud_out}",
    );
    assert!(
        (loud_out.contains("> greet") && loud_out.contains("> echo SENTINEL-OUT"))
            || loud_err.contains("npm notice run"),
        "npm host banner expected on stdout (npm 11) or stderr (npm 12). stdout: {loud_out}; \
         stderr: {loud_err}",
    );

    let quiet = run_in(
        proj.path(),
        &[("NPM_CONFIG_LOGLEVEL", "notice")],
        &["-q", "greet"],
    );
    let quiet_out = String::from_utf8_lossy(&quiet.stdout);
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        quiet.status.success(),
        "run -q greet should succeed. stderr: {}",
        String::from_utf8_lossy(&quiet.stderr),
    );
    assert!(
        quiet_out.lines().any(|line| line == "SENTINEL-OUT"),
        "the task's own output must survive -q. stdout: {quiet_out}",
    );
    assert!(
        (quiet_out.contains("> greet") && quiet_out.contains("> echo SENTINEL-OUT"))
            || quiet_err.contains("npm notice run"),
        "-q must preserve npm diagnostics. stdout: {quiet_out}; stderr: {quiet_err}",
    );

    let very_quiet = run_in(
        proj.path(),
        &[("NPM_CONFIG_LOGLEVEL", "notice")],
        &["-qq", "greet"],
    );
    let very_quiet_out = String::from_utf8_lossy(&very_quiet.stdout);
    let very_quiet_err = String::from_utf8_lossy(&very_quiet.stderr);
    assert!(very_quiet.status.success());
    assert!(very_quiet_out.lines().any(|line| line == "SENTINEL-OUT"));
    assert!(!very_quiet_out.contains("> greet"));
    assert!(!very_quiet_err.contains("npm notice run"));
}

/// #93 acceptance: a script that calls `runner` again inherits the quiet level
/// via the exported `RUNNER_QUIET`, so the nested npm banner is silenced too.
#[test]
fn host_quiet_propagates_to_nested_runner() {
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

    let output = run_in(proj.path(), &[], &["-qq", "build"]);
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

#[test]
fn every_level_preserves_task_streams_and_clamps_at_mute() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("streams").file(
        "Makefile",
        "greet:\n\t@printf 'TASK-OUT\\n'\n\t@printf 'TASK-ERR\\n' >&2\n",
    );
    let levels: &[&[&str]] = &[&[], &["-q"], &["-qq"], &["-qqq"], &["-qqqq"], &["-qqqqqq"]];
    for flags in levels {
        let mut args = flags.to_vec();
        args.push("greet");
        let output = run_in(proj.path(), &[], &args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "flags={flags:?}; stderr={stderr}");
        assert!(
            stdout.contains("TASK-OUT"),
            "flags={flags:?}; stdout={stdout}"
        );
        assert!(
            stderr.contains("TASK-ERR"),
            "flags={flags:?}; stderr={stderr}"
        );
        if !flags.is_empty() {
            assert!(!stderr.contains('→'), "flags={flags:?}; stderr={stderr}");
        }
    }
}

#[test]
fn mute_hides_fatal_text_but_preserves_exit_status() {
    let proj = npm_project("mute-fatal");
    let shown = run_in(proj.path(), &[], &["-qqq", "package.json:missing"]);
    let muted = run_in(proj.path(), &[], &["-qqqq", "package.json:missing"]);
    assert!(!shown.status.success());
    assert!(!muted.status.success());
    assert_ne!(shown.stderr.len(), 0);
    assert!(
        muted.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&muted.stderr)
    );
}

#[test]
fn configured_errors_false_hides_post_resolution_failure() {
    let proj = npm_project("configured-errors").file("runner.toml", "[output]\nerrors = false\n");
    let output = run_in(proj.path(), &[], &["package.json:missing"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout.len(), 0);
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn quiet_dashboard_emits_no_operational_output() {
    let proj = make_project("quiet-dashboard");
    let output = run_in(proj.path(), &[], &["-q"]);
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 0);
    assert_eq!(output.stderr.len(), 0);
}

#[test]
fn quiet_clean_emits_no_operational_output() {
    let proj = npm_project("quiet-clean").dir("node_modules");
    let output = runner_in(proj.path(), &[], &["-q", "clean"]);
    assert!(!output.status.success());
    assert_eq!(output.stdout.len(), 0);
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires --yes"));

    let confirmed = runner_in(proj.path(), &[], &["-q", "clean", "--yes"]);
    assert!(confirmed.status.success());
    assert_eq!(confirmed.stdout.len(), 0);
    assert_eq!(confirmed.stderr.len(), 0);
}

#[test]
fn quiet_clean_without_targets_emits_no_operational_output() {
    let proj = make_project("quiet-clean-empty");
    let output = run_in(proj.path(), &[], &["-q", "clean"]);
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 0);
    assert_eq!(output.stderr.len(), 0);
}

#[test]
fn task_streams_require_explicit_discard_config() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("discard")
        .file(
            "Makefile",
            "greet:\n\t@printf 'TASK-OUT\\n'\n\t@printf 'TASK-ERR\\n' >&2\n",
        )
        .file(
            "runner.toml",
            "[tasks.greet.output.task]\nstdout = false\nstderr = true\n",
        );
    let output = run_in(proj.path(), &[], &["-qqqq", "greet"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("TASK-OUT"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("TASK-ERR"));
}

#[test]
fn qualified_task_settings_override_bare_task_settings_per_axis() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("qualified-settings")
        .file(
            "Makefile",
            "greet:\n\t@printf 'TASK-OUT\\n'\n\t@printf 'TASK-ERR\\n' >&2\n",
        )
        .file(
            "runner.toml",
            "[tasks.greet.output.task]\nstdout = false\nstderr = \
             false\n[tasks.\"make:greet\".output.task]\nstdout = true\n",
        );
    let output = run_in(proj.path(), &[], &["-q", "make:greet"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("TASK-OUT"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("TASK-ERR"));
}

#[test]
fn fqn_task_uses_qualified_stream_settings() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("fqn-settings")
        .file("Makefile", "greet:\n\t@printf 'TASK-OUT\\n'\n")
        .file(
            "runner.toml",
            "[tasks.\"root:make#greet\".output.task]\nstdout = false\n",
        );
    let output = run_in(proj.path(), &[], &["-q", "root:make#greet"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("TASK-OUT"));
}

#[test]
fn explain_reports_unsupported_host_reduction() {
    if !tool_available("just") {
        eprintln!("skipping: `just` not found on PATH");
        return;
    }
    let proj = TempProject::new("just-explain").file("justfile", "greet:\n  @echo TASK-OUT\n");
    let output = run_in(proj.path(), &[], &["-qqq", "--dry-run", "greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("host: just diagnostics=reduced applied=normal"));
    assert!(stderr.contains("matrix=just"));
    assert!(stderr.contains("--quiet suppresses task output"));
}

#[test]
fn runner_and_host_config_axes_are_independent() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let runner_only = TempProject::new("runner-axis")
        .file(
            "package.json",
            "{ \"scripts\": { \"greet\": \"echo SENTINEL-OUT\" } }\n",
        )
        .file("package-lock.json", "{}\n")
        .file("runner.toml", "[output]\nprogress = false\n");
    let runner_output = run_in(
        runner_only.path(),
        &[("NPM_CONFIG_LOGLEVEL", "notice")],
        &["greet"],
    );
    let runner_stdout = String::from_utf8_lossy(&runner_output.stdout);
    let runner_stderr = String::from_utf8_lossy(&runner_output.stderr);
    assert!(runner_output.status.success());
    assert!(!runner_stderr.contains('→'));
    assert!(
        (runner_stdout.contains("> greet") && runner_stdout.contains("> echo SENTINEL-OUT"))
            || runner_stderr.contains("npm notice run"),
        "runner category must not quiet host. stdout: {runner_stdout}; stderr: {runner_stderr}",
    );

    let host_only = TempProject::new("host-axis")
        .file(
            "package.json",
            "{ \"scripts\": { \"greet\": \"echo SENTINEL-OUT\" } }\n",
        )
        .file("package-lock.json", "{}\n")
        .file("runner.toml", "[output.tool]\nquiet = true\n");
    let host_output = run_in(
        host_only.path(),
        &[("NPM_CONFIG_LOGLEVEL", "notice")],
        &["greet"],
    );
    let host_stdout = String::from_utf8_lossy(&host_output.stdout);
    let host_stderr = String::from_utf8_lossy(&host_output.stderr);
    assert!(host_output.status.success());
    assert!(host_stderr.contains('→'));
    assert!(!host_stdout.contains("> greet"));
    assert!(!host_stderr.contains("npm notice run"));
}

#[test]
fn quiet_preset_overrides_only_the_settings_it_expands_to() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let quiet_task = TempProject::new("quiet-preset-keeps")
        .file(
            "package.json",
            "{ \"scripts\": { \"greet\": \"echo SENTINEL-OUT\" } }\n",
        )
        .file("package-lock.json", "{}\n")
        .file("runner.toml", "[tasks.greet.output.tool]\nquiet = true\n");
    let kept = run_in(quiet_task.path(), &[], &["-q", "--dry-run", "greet"]);
    let kept_stderr = String::from_utf8_lossy(&kept.stderr);
    assert!(kept.status.success(), "stderr: {kept_stderr}");
    assert!(
        kept_stderr.contains("diagnostics=quiet applied=quiet args=[--silent]"),
        "-q leaves the tool alone. stderr: {kept_stderr}",
    );

    let loud_task = TempProject::new("quiet-preset-overrides")
        .file(
            "package.json",
            "{ \"scripts\": { \"greet\": \"echo SENTINEL-OUT\" } }\n",
        )
        .file("package-lock.json", "{}\n")
        .file("runner.toml", "[tasks.greet.output.tool]\nquiet = false\n");
    let configured = run_in(loud_task.path(), &[], &["--dry-run", "greet"]);
    let configured_stderr = String::from_utf8_lossy(&configured.stderr);
    assert!(configured.status.success(), "stderr: {configured_stderr}");
    assert!(configured_stderr.contains("diagnostics=normal applied=normal args=[]"));

    let explicit = run_in(loud_task.path(), &[], &["-qq", "--dry-run", "greet"]);
    let explicit_stderr = String::from_utf8_lossy(&explicit.stderr);
    assert!(explicit.status.success(), "stderr: {explicit_stderr}");
    assert!(
        explicit_stderr.contains("diagnostics=quiet applied=quiet args=[--silent]"),
        "-qq quiets the tool over the task. stderr: {explicit_stderr}",
    );
}

#[test]
fn a_weaker_command_line_preset_keeps_the_environment_presets_other_settings() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let project = npm_project("quiet-layers");
    let both = runner_in(
        project.path(),
        &[("RUNNER_QUIET", "2")],
        &["-q", "run", "--dry-run", "greet"],
    );
    let stderr = String::from_utf8_lossy(&both.stderr);
    assert!(both.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("warnings=hide") && stderr.contains("args=[--silent]"),
        "RUNNER_QUIET=2 still hides warnings and quiets the tool. stderr: {stderr}",
    );
    let restored = runner_in(
        project.path(),
        &[("RUNNER_QUIET", "2")],
        &["-q", "--warnings", "run", "--dry-run", "greet"],
    );
    let stderr = String::from_utf8_lossy(&restored.stderr);
    assert!(restored.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("warnings=show") && stderr.contains("args=[--silent]"),
        "--warnings restores only warnings. stderr: {stderr}",
    );
}

#[test]
fn task_tool_quiet_outranks_project_tool_quiet() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let proj = TempProject::new("host-normal-precedence")
        .file(
            "package.json",
            "{ \"scripts\": { \"greet\": \"echo SENTINEL-OUT\" } }\n",
        )
        .file("package-lock.json", "{}\n")
        .file(
            "runner.toml",
            "[output.tool]\nquiet = true\n[tasks.greet.output.tool]\nquiet = false\n",
        );
    let output = run_in(proj.path(), &[], &["--dry-run", "greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("diagnostics=normal applied=normal args=[]"));
}

#[test]
fn quiet_parallel_preserves_streams_without_runner_prefixes() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("parallel-streams").file(
        "Makefile",
        "one:\n\t@printf 'ONE-OUT\\n'\n\t@printf 'ONE-ERR\\n' >&2\ntwo:\n\t@printf \
         'TWO-OUT\\n'\n\t@printf 'TWO-ERR\\n' >&2\n",
    );
    let output = run_in(proj.path(), &[], &["-q", "-p", "one", "two"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("ONE-OUT") && stdout.contains("TWO-OUT"));
    assert!(stderr.contains("ONE-ERR") && stderr.contains("TWO-ERR"));
    assert!(!stdout.contains("[one]") && !stdout.contains("[two]"));
    assert!(!stderr.contains("[one]") && !stderr.contains("[two]"));
}

#[test]
fn explain_reports_exact_applied_host_args() {
    if !tool_available("npm") {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let proj = npm_project("explain-host-args");
    let output = run_in(proj.path(), &[], &["-qq", "--dry-run", "greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("host: npm diagnostics=quiet applied=quiet args=[--silent]"));
}

#[test]
fn explain_reports_output_policy_for_local_files_without_running_them() {
    if !tool_available("python3") {
        eprintln!("skipping: `python3` not found on PATH");
        return;
    }
    let proj = TempProject::new("local-file-explain").file("tool.py", "print('PY-OUT')\n");
    let output = run_in(proj.path(), &[], &["-qq", "--dry-run", "./tool.py"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stdout.lines().any(|line| line == "PY-OUT"),
        "--dry-run stops after the plan. stdout: {stdout}"
    );
    assert!(
        stderr.contains("output: level=very-quiet progress=hide warnings=hide errors=show"),
        "stderr: {stderr}",
    );
    assert!(
        stderr.contains("task.stdout=inherit task.stderr=inherit"),
        "stderr: {stderr}",
    );
}

#[test]
fn explicit_quiet_keeps_config_warning_suppression() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let loud = make_project("quiet-config-warn-control").file("runner.toml", "[bogus]\nx = 1\n");
    let control = run_in(loud.path(), &[], &["-q", "greet"]);
    let control_err = String::from_utf8_lossy(&control.stderr);
    assert!(control.status.success(), "stderr: {control_err}");
    assert!(
        control_err.contains("unknown key `bogus`"),
        "-q alone keeps non-fatal warnings. stderr: {control_err}",
    );

    let proj = make_project("quiet-config-warn").file(
        "runner.toml",
        "[output]\nwarnings = false\n\n[bogus]\nx = 1\n",
    );
    let output = run_in(proj.path(), &[], &["-q", "greet"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("unknown key `bogus`"),
        "[output] warnings = false must survive an explicit -q. stderr: {stderr}",
    );
}

#[test]
fn per_task_progress_switch_hides_only_that_tasks_arrow() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("per-task-progress")
        .file("Makefile", "greet:\n\t@true\nother:\n\t@true\n")
        .file("runner.toml", "[tasks.greet.output]\nprogress = false\n");

    let hidden = run_in(proj.path(), &[], &["greet"]);
    let hidden_err = String::from_utf8_lossy(&hidden.stderr);
    assert!(hidden.status.success(), "stderr: {hidden_err}");
    assert!(
        !hidden_err.contains("→"),
        "greet's arrow must be hidden by its own setting. stderr: {hidden_err}",
    );

    let shown = run_in(proj.path(), &[], &["other"]);
    let shown_err = String::from_utf8_lossy(&shown.stderr);
    assert!(shown.status.success(), "stderr: {shown_err}");
    assert!(
        shown_err.contains("→ make other"),
        "other's arrow is untouched. stderr: {shown_err}",
    );

    let explain = run_in(proj.path(), &[], &["--dry-run", "greet"]);
    let explain_err = String::from_utf8_lossy(&explain.stderr);
    assert!(
        explain_err.contains("output: level=off progress=hide"),
        "--dry-run reports the per-task effective value. stderr: {explain_err}",
    );
}

#[test]
fn per_task_timing_switch_hides_only_that_tasks_chain_line() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("per-task-timing")
        .file("Makefile", "greet:\n\t@true\nother:\n\t@true\n")
        .file("runner.toml", "[tasks.greet.output]\ntiming = false\n");
    let output = command_in(
        runner_binary(),
        proj.path(),
        &[(GITHUB_ACTIONS, "")],
        &["run", "-s", "greet", "other"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert_eq!(
        stderr.matches("finished in").count(),
        1,
        "only `other` reports timing. stderr: {stderr}",
    );
    assert!(stderr.contains("other finished in"), "stderr: {stderr}");
}

#[test]
fn mute_hides_clap_parse_errors() {
    let proj = make_project("mute-clap");
    let output = run_in(proj.path(), &[], &["-qqqq", "--definitely-invalid"]);
    assert!(!output.status.success());
    assert_eq!(output.stdout.len(), 0);
    assert_eq!(output.stderr.len(), 0);

    let clustered = run_in(proj.path(), &[], &["-qqqqZ"]);
    assert!(!clustered.status.success());
    assert_eq!(clustered.stdout.len(), 0);
    assert_eq!(clustered.stderr.len(), 0);
}

#[test]
fn qualified_parallel_task_uses_resolved_stream_policy() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("qualified-stream")
        .file(
            "Makefile",
            "one:\n\t@printf 'ONE-OUT\\n'\ntwo:\n\t@printf 'TWO-OUT\\n'\n",
        )
        .file(
            "runner.toml",
            "[tasks.one.output.task]\nstdout = false\n[tasks.two.output.task]\nstdout = true\n",
        );
    let output = run_in(proj.path(), &[], &["-q", "-p", "make:one", "make:two"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(!stdout.contains("ONE-OUT"), "stdout: {stdout}");
    assert!(stdout.contains("TWO-OUT"), "stdout: {stdout}");
}

#[test]
fn summary_can_be_the_only_runner_output() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let proj = TempProject::new("summary-only")
        .file("Makefile", "one:\n\t@true\ntwo:\n\t@true\n")
        .file(
            "runner.toml",
            "[output]\nprogress = false\nwarnings = false\nerrors = false\ngroups = false\ntiming \
             = false\nsummary = true\n[tasks.one.output.task]\nstdout = false\nstderr = \
             false\n[tasks.two.output.task]\nstdout = false\nstderr = false\n",
        );
    let output = run_in(proj.path(), &[], &["-s", "one", "two"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("summary: 2 tasks, 2 ok"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("finished in"), "stderr: {stderr}");
    assert!(!stderr.contains('→'), "stderr: {stderr}");
}
