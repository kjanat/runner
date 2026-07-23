//! Integration coverage for `--runtime` (#94).
//!
//! The point of the axis is the third row of the issue's table: forcing the
//! *process tree* onto a runtime, not just picking who invokes the script.
//! That is only observable by running a script that asks which runtime it is
//! on, so these drive the real binaries.
//!
//! Skips when the runtime under test is not installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "runner-runtime-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp project dir");
        Self { path }
    }

    fn file(self, name: &str, contents: &str) -> Self {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write project file");
        self
    }

    /// Write a file and mark it executable, so dispatch sees the exec bit and
    /// the `#!` line the real-world cases carry.
    fn executable(self, name: &str, contents: &str) -> Self {
        let this = self.file(name, contents);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(this.path.join(name), std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x");
        }
        this
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

/// Assert a spawned dispatch exited cleanly and its stdout names `needle`.
///
/// A bare `stdout.contains(...)` check reports only empty stdout when the
/// spawned runtime failed, hiding the real error on the child's stderr; this
/// surfaces the exit status and both streams so a CI-only failure is
/// diagnosable from the log alone.
fn assert_stdout_has(output: &Output, needle: &str, context: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(needle),
        "{context}\n  status: {}\n  stdout: {stdout}\n  stderr: {stderr}",
        output.status,
    );
}

fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The real bin directories of any mise-managed tools, resolved once where a
/// mise config exists (the crate root) via `mise bin-paths`.
///
/// A mise shim resolves its version by walking up from the process's cwd for a
/// config. These tests spawn tools with cwd set to a throwaway project outside
/// the repo, where no config exists, so a shimmed tool (bun/deno on CI) dies
/// with `No version is set for shim`. Putting the real bin dirs on the child
/// `PATH` ahead of the shims makes the tool resolve independently of cwd.
/// Empty when mise is absent, so a machine with the tools directly on `PATH`
/// is unaffected.
fn mise_bin_paths() -> Vec<PathBuf> {
    Command::new("mise")
        .arg("bin-paths")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|line| PathBuf::from(line.trim()))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// `PATH` for a spawned dispatch: the `run` binary's own dir first (so a nested
/// `run` reaches this build), then the real mise tool dirs (so shimmed
/// runtimes resolve outside the repo), then the inherited `PATH`.
fn child_path(bin_dir: &Path) -> std::ffi::OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let entries = std::iter::once(bin_dir.to_path_buf())
        .chain(mise_bin_paths())
        .chain(std::env::split_paths(&inherited));
    std::env::join_paths(entries).expect("PATH joins")
}

/// The `run` binary aimed at `dir`, with `RUNNER_*` scrubbed and the binary's
/// own directory on `PATH` so a nested `run` reaches this build. Callers that
/// need extra environment build on this; the rest use [`run_in`].
fn runner_command(dir: &Path) -> Command {
    let binary = run_binary();
    let bin_dir = binary.parent().expect("binary lives in a directory");
    let joined = child_path(bin_dir);

    let mut cmd = Command::new(&binary);
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RUNNER_")
        {
            cmd.env_remove(&key);
        }
    }
    cmd.env("PATH", joined).arg("--dir").arg(dir);
    cmd
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    runner_command(dir)
        .args(args)
        .output()
        .expect("run should execute")
}

/// Run with `PATH` holding only the binary under test, so whatever dispatch
/// picks is unspawnable and nothing reaches a registry or a real task runner.
/// The `\u{2192}` dispatch arrow is written before the spawn, so assertions on
/// *what would have run* still hold.
fn arrow_only(dir: &Path, args: &[&str]) -> Output {
    let binary = run_binary();
    let bin_dir = binary.parent().expect("binary lives in a directory");
    Command::new(&binary)
        .env("PATH", bin_dir)
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("run should execute")
}

/// An npm project (npm lockfile, so the resolver picks npm) whose `which`
/// script asks the runtime it is executing on. Without `--runtime` this
/// reports NODE; the whole feature is making it report BUN.
fn probe_project(tag: &str) -> TempProject {
    TempProject::new(tag)
        .file(
            "package.json",
            r#"{ "name": "rt", "scripts": { "which": "node -e \"console.log(process.versions.bun ? 'BUN' : 'NODE')\"", "outer": "run -q which" } }"#,
        )
        .file("package-lock.json", "{}")
        .file(
            "probe.js",
            "console.log(process.versions.bun ? 'BUN' : 'NODE');\n",
        )
}

#[test]
fn runtime_bun_forces_the_scripts_process_tree_onto_bun() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    let proj = probe_project("bun");

    // Control: an npm project runs the script's `node` on real Node.
    let plain = run_in(proj.path(), &["-q", "which"]);
    assert_stdout_has(&plain, "NODE", "control should report NODE");

    // #94: `bun run` alone would still hand that `node` to system Node;
    // `bun --bun run` is what moves it.
    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "which"]);
    assert_stdout_has(
        &forced,
        "BUN",
        "--runtime bun must put the script's node on bun",
    );
}

#[test]
fn runtime_survives_a_nested_run() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    // `outer` shells back into `run`; the runtime must not fall back to the
    // detected PM at the nested boundary.
    let proj = probe_project("nested");
    let output = run_in(proj.path(), &["-q", "--runtime", "bun", "outer"]);

    assert_stdout_has(&output, "BUN", "nested dispatch must inherit the runtime");
}

#[test]
fn runtime_also_selects_the_local_file_runtime() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    // Local files already honoured `--pm bun`; the dedicated axis must reach
    // them too, or the two dispatch paths disagree about the same word.
    let proj = probe_project("local");

    let plain = run_in(proj.path(), &["-q", "probe.js"]);
    assert_stdout_has(&plain, "NODE", "control should report NODE");

    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "probe.js"]);
    assert_stdout_has(&forced, "BUN", "--runtime bun must run the file on bun");
}

#[test]
fn config_layer_sets_the_runtime() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    let proj = probe_project("config").file("runner.toml", "[runtime]\njs = \"bun\"\n");
    let output = run_in(proj.path(), &["-q", "which"]);

    assert_stdout_has(&output, "BUN", "[runtime].js must apply without a flag");
}

#[test]
fn explain_names_the_runtime_and_where_it_came_from() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    let proj = probe_project("explain");
    let output = run_in(proj.path(), &["--explain", "--runtime", "bun", "which"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("bun via --runtime"),
        "--explain must attribute the runtime to its source. stderr: {stderr}",
    );
}

#[test]
fn an_unknown_runtime_is_rejected_with_the_valid_set() {
    let proj = probe_project("bad");
    let output = run_in(proj.path(), &["--runtime", "zoot", "which"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("zoot") && stderr.contains("bun"),
        "the error must name the bad value and the valid ones. stderr: {stderr}",
    );
}

/// The `#!/usr/bin/env node` probe every `node_modules/.bin` entry is shaped
/// like: an executable file whose shebang would otherwise pin it to Node.
const SHEBANG_PROBE: &str =
    "#!/usr/bin/env node\nconsole.log(process.versions.bun ? 'BUN' : 'NODE');\n";

#[test]
fn runtime_outranks_a_files_node_shebang() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    // The flag's advertised case. `build_command` used to return at the
    // shebang branch before anything read the runtime, so `--runtime bun`
    // on a `#!/usr/bin/env node` file dispatched `node`.
    let proj = probe_project("shebang").executable("sheb.js", SHEBANG_PROBE);

    let plain = run_in(proj.path(), &["-q", "./sheb.js"]);
    assert_stdout_has(&plain, "NODE", "control should honour the shebang");

    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "./sheb.js"]);
    assert_stdout_has(
        &forced,
        "BUN",
        "--runtime bun must outrank the file's shebang",
    );
}

#[test]
fn runtime_reaches_an_extensionless_executable() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    // No extension to route on: only the shebang says this is JS at all.
    let proj = probe_project("noext").executable("tool", SHEBANG_PROBE);

    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "./tool"]);

    assert_stdout_has(
        &forced,
        "BUN",
        "--runtime bun must reach an extensionless node-shebanged file",
    );
}

#[test]
fn runtime_reaches_a_locally_installed_dependency_bin() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    // npm bins carry a node shebang by construction, so the shebang branch
    // swallowed every one of them.
    let proj = probe_project("localdep")
        .file(
            "node_modules/probebin/package.json",
            r#"{ "name": "probebin", "bin": { "probebin": "./cli.js" } }"#,
        )
        .executable("node_modules/probebin/cli.js", SHEBANG_PROBE);

    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "probebin"]);

    assert_stdout_has(&forced, "BUN", "--runtime bun must reach a dependency bin");
}

#[test]
fn runtime_does_not_hijack_a_non_js_file() {
    if !tool_available("bun") {
        eprintln!("skipping: bun not found on PATH");
        return;
    }
    #[cfg(not(unix))]
    {
        eprintln!("skipping: needs a POSIX shell");
        return;
    }
    #[cfg(unix)]
    {
        let proj = probe_project("shell").executable("script.sh", "#!/bin/sh\necho SHELL-OK\n");

        let output = run_in(proj.path(), &["-q", "--runtime", "bun", "./script.sh"]);

        assert_stdout_has(
            &output,
            "SHELL-OK",
            "a JS runtime must not be forced onto a shell script",
        );
    }
}

#[test]
fn runtime_selects_the_exec_fallback_primitive() {
    // No task, no file, no installed dependency: the token goes to a package
    // exec primitive, which used to be the resolved PM's regardless.
    let proj = probe_project("exec");

    for (runtime, expected) in [("node", "npx"), ("bun", "bunx"), ("deno", "deno x")] {
        let output = arrow_only(
            proj.path(),
            &["--runtime", runtime, "definitely-not-a-real-tool-xyz"],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "\u{2192} {expected} definitely-not-a-real-tool-xyz"
            )),
            "--runtime {runtime} must exec through {expected}. stderr: {stderr}",
        );
    }
}

#[test]
fn runtime_decides_the_bun_test_fallback() {
    // The fallback keyed only off the resolved PM, so `--runtime node` in a
    // bun project still landed on bun's built-in test runner.
    let proj = TempProject::new("buntest")
        .file("package.json", r#"{ "name": "bt" }"#)
        .file("bun.lock", "");

    let forced_bun = arrow_only(proj.path(), &["--runtime", "bun", "test"]);
    assert!(
        String::from_utf8_lossy(&forced_bun.stderr).contains("\u{2192} bun test"),
        "--runtime bun must reach `bun test`. stderr: {}",
        String::from_utf8_lossy(&forced_bun.stderr),
    );

    let forced_node = arrow_only(proj.path(), &["--runtime", "node", "test"]);
    assert!(
        !String::from_utf8_lossy(&forced_node.stderr).contains("\u{2192} bun test"),
        "--runtime node must not reach bun's test runner. stderr: {}",
        String::from_utf8_lossy(&forced_node.stderr),
    );
}

#[test]
fn a_forced_runtime_outranks_turbo_in_source_selection() {
    // turbo.json outranks package.json at the default tier, so in any
    // turborepo the flag was a guaranteed silent no-op.
    let proj = probe_project("turbo").file("turbo.json", r#"{ "tasks": { "which": {} } }"#);

    let plain = arrow_only(proj.path(), &["which"]);
    assert!(
        String::from_utf8_lossy(&plain.stderr).contains("\u{2192} turbo which"),
        "control should pick the turbo task. stderr: {}",
        String::from_utf8_lossy(&plain.stderr),
    );

    let forced = arrow_only(proj.path(), &["--runtime", "bun", "which"]);
    assert!(
        String::from_utf8_lossy(&forced.stderr).contains("\u{2192} package.json which"),
        "a forced runtime must bias selection toward the script it can run. stderr: {}",
        String::from_utf8_lossy(&forced.stderr),
    );
}

#[test]
fn a_source_that_cannot_honour_the_runtime_says_so() {
    // The invariant: get the runtime, or be told it did not apply.
    let proj = TempProject::new("unhonored").file("justfile", "build:\n\t@echo JUST-RAN\n");

    let output = arrow_only(proj.path(), &["--explain", "--runtime", "bun", "build"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("warn:") && stderr.contains("--runtime bun was not applied"),
        "an unhonourable runtime must warn. stderr: {stderr}",
    );
    assert!(
        stderr.contains("just"),
        "the warning must name the source that won. stderr: {stderr}",
    );
    assert!(
        stderr.contains("\u{b7} runner runtime bun not applied"),
        "--explain must carry the same fact. stderr: {stderr}",
    );
}

#[test]
fn runtime_node_dispatches_node_run_instead_of_erroring() {
    if !tool_available("node") {
        eprintln!("skipping: node not found on PATH");
        return;
    }
    // `--runtime node` used to resolve a node PM and hard-error in a
    // Deno-resolved project, contradicting its own documentation.
    let proj = TempProject::new("node-in-deno")
        .file("deno.json", r#"{ "nodeModulesDir": "none" }"#)
        .file(
            "package.json",
            r#"{ "name": "nd", "scripts": { "which": "node -e \"console.log('NODE-RAN')\"" } }"#,
        );

    let output = run_in(proj.path(), &["-q", "--runtime", "node", "which"]);

    assert!(
        output.status.success(),
        "--runtime node must not need a node package manager. stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_stdout_has(&output, "NODE-RAN", "the script should have run");
}

#[test]
fn every_runtime_forwards_user_args_identically() {
    #[cfg(not(unix))]
    {
        eprintln!("skipping: the echo script needs a POSIX shell");
        return;
    }
    #[cfg(unix)]
    {
        // node needs an injected `--`, deno must not get one, bun needs
        // neither. Injecting uniformly corrupts deno's argv; injecting
        // nowhere makes node exit with `bad option`.
        let proj = TempProject::new("args")
            .file(
                "package.json",
                r#"{ "name": "args", "scripts": { "echoargs": "./echo.sh" } }"#,
            )
            .executable("echo.sh", "#!/bin/sh\necho \"ARGS:[$*]\"\n");

        for runtime in ["node", "bun", "deno"] {
            if !tool_available(runtime) {
                eprintln!("skipping {runtime}: not found on PATH");
                continue;
            }
            let with_args = run_in(
                proj.path(),
                &["-q", "--runtime", runtime, "echoargs", "--flag", "val"],
            );
            assert_stdout_has(
                &with_args,
                "ARGS:[--flag val]",
                &format!("{runtime} must forward args verbatim"),
            );

            let no_args = run_in(proj.path(), &["-q", "--runtime", runtime, "echoargs"]);
            assert_stdout_has(
                &no_args,
                "ARGS:[]",
                &format!("{runtime} must not invent an argument"),
            );
        }
    }
}

#[test]
fn runtime_node_warns_about_the_lifecycle_scripts_it_skips() {
    if !tool_available("node") {
        eprintln!("skipping: node not found on PATH");
        return;
    }
    // `node --run` omits pre/post scripts by design, unlike npm run, bun run
    // and deno task. Silent omission turns a generated-source `prebuild` into
    // a stale build that fails somewhere else.
    let proj = TempProject::new("lifecycle").file(
        "package.json",
        r#"{ "name": "lc", "scripts": { "prebuild": "node -e \"0\"", "build": "node -e \"console.log('BUILT')\"" } }"#,
    );

    let output = run_in(proj.path(), &["--runtime", "node", "build"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("prebuild"),
        "the warning must name the script that will not run. stderr: {stderr}",
    );

    // The other two runtimes do run them, so they must stay quiet.
    if tool_available("bun") {
        let quiet = run_in(proj.path(), &["--runtime", "bun", "build"]);
        assert!(
            !String::from_utf8_lossy(&quiet.stderr).contains("does not run"),
            "bun run executes pre/post; no warning is due. stderr: {}",
            String::from_utf8_lossy(&quiet.stderr),
        );
    }
}
