//! Integration coverage for dispatch across nested `runner`/`run` processes.
//!
//! The three behaviours here only show up once a task re-enters runner
//! through a package script, so each test builds a throwaway project, puts
//! the `run` alias binary on the child `PATH`, and drives a real process
//! tree:
//!
//! - argument forwarding survives the nested boundary (#89);
//! - a task that resolves back to itself is refused instead of forking
//!   until the process tree collapses (#90);
//! - an installed dependency resolves to the binary its manifest declares
//!   rather than being handed to `npx` as a registry spec (#91).
//!
//! Unix-only: the fixtures are `#!`-scripts marked executable, which is how
//! a real `node_modules` bin and a `make` recipe reach the shell.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Self-cleaning temp project.
struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "runner-nested-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp project dir");
        Self { path }
    }

    fn file(self, name: &str, contents: &str) -> Self {
        self.write(name, contents, false);
        self
    }

    fn script(self, name: &str, contents: &str) -> Self {
        self.write(name, contents, true);
        self
    }

    fn write(&self, name: &str, contents: &str, executable: bool) {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write project file");
        if executable {
            let mut perms = std::fs::metadata(&path).expect("stat").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod +x");
        }
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

/// Run the `run` binary against `dir` with every `RUNNER_*` var scrubbed and
/// the binary's own directory prepended to `PATH`, so a task that shells out
/// to `run` reaches this build rather than an installed one.
fn run_in(dir: &Path, args: &[&str]) -> Output {
    let binary = run_binary();
    let bin_dir = binary.parent().expect("binary lives in a directory");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let joined = std::env::join_paths(
        std::iter::once(bin_dir.to_path_buf()).chain(std::env::split_paths(&path)),
    )
    .expect("PATH joins");

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
    cmd.env("PATH", joined)
        .arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("run should execute")
}

#[test]
fn flags_that_collide_with_runners_own_reach_the_task() {
    // #89: `-p` belongs to the tool, but runner bound it to `--parallel`,
    // entered chain mode, and then rejected `--noEmit` as a non-task
    // positional.
    let proj = TempProject::new("forward").script("show.sh", "#!/bin/sh\necho \"show got: $*\"\n");

    let output = run_in(
        proj.path(),
        &["./show.sh", "-p", "tsconfig.json", "--noEmit"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("show got: -p tsconfig.json --noEmit"),
        "every flag must reach the tool verbatim. stdout: {stdout}",
    );
}

#[test]
fn forwarding_survives_a_nested_run_without_extra_delimiters() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    // The shape from #89: a task delegates through `run`, and the flags it
    // passes must not be reparsed by the nested process. Previously this
    // needed a delimiter whose count the user had to guess.
    let proj = TempProject::new("nested-forward")
        .file(
            "Makefile",
            "wrap:\n\t@run -q ./show.sh -p tsconfig.json --noEmit\n",
        )
        .script("show.sh", "#!/bin/sh\necho \"show got: $*\"\n");

    let output = run_in(proj.path(), &["wrap"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("show got: -p tsconfig.json --noEmit"),
        "the nested run must forward, not reparse. stdout: {stdout}",
    );
}

#[test]
fn a_task_that_resolves_back_to_itself_is_refused() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    // #90: this used to spawn copies of itself until the process tree
    // collapsed.
    let proj = TempProject::new("cycle").file("Makefile", "loop:\n\t@run -q loop\n");

    let output = run_in(proj.path(), &["loop"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "a cycle must not exit clean");
    assert!(
        stderr.contains("recursive task resolution detected"),
        "stderr: {stderr}",
    );
    assert!(
        stderr.contains("make:loop -> make:loop"),
        "the diagnostic must name the loop. stderr: {stderr}",
    );
}

#[test]
fn quiet_does_not_disable_cycle_detection() {
    if !tool_available("make") {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    // `--quiet` hides dispatch output; it must never hide a safety
    // diagnostic, which is what made the recursion so hard to spot.
    let proj = TempProject::new("cycle-quiet").file("Makefile", "loop:\n\t@run -q loop\n");

    let output = run_in(proj.path(), &["-q", "loop"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "a cycle must not exit clean");
    assert!(
        stderr.contains("recursive task resolution detected"),
        "stderr: {stderr}",
    );
}

/// Project whose only dependency is an npm alias exposing a single binary,
/// the shape from #91 (`"@typescript/native": "npm:typescript@^7"`).
fn aliased_dependency_project(tag: &str) -> TempProject {
    TempProject::new(tag)
        .file(
            "package.json",
            r#"{ "devDependencies": { "@typescript/native": "npm:typescript@^7" } }"#,
        )
        .file("package-lock.json", "{}")
        .file(
            "node_modules/@typescript/native/package.json",
            r#"{ "name": "typescript", "version": "7.0.2", "bin": { "tsc": "./bin/tsc" } }"#,
        )
        .script(
            "node_modules/@typescript/native/bin/tsc",
            "#!/bin/sh\necho \"tsc got: $*\"\n",
        )
}

#[test]
fn a_scoped_alias_resolves_to_the_binary_its_manifest_declares() {
    let proj = aliased_dependency_project("alias");

    let output = run_in(proj.path(), &["@typescript/native", "--noEmit"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("tsc got: --noEmit"),
        "the declared binary should run with the args forwarded. stdout: {stdout}",
    );
}

#[test]
fn explain_names_the_local_package_and_the_binary_it_picked() {
    let proj = aliased_dependency_project("alias-explain");

    let output = run_in(
        proj.path(),
        &["--explain", "@typescript/native", "--noEmit"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("tsc from") && stderr.contains("local dependency"),
        "--explain must show which binary of which package ran. stderr: {stderr}",
    );
    assert!(
        stderr.contains("@typescript/native"),
        "--explain must name the package directory. stderr: {stderr}",
    );
}

#[test]
fn a_dependency_with_no_binary_says_so_instead_of_reaching_the_registry() {
    let proj = TempProject::new("no-bin")
        .file(
            "package.json",
            r#"{ "dependencies": { "left-pad": "^1" } }"#,
        )
        .file("package-lock.json", "{}")
        .file(
            "node_modules/left-pad/package.json",
            r#"{ "name": "left-pad", "version": "1.3.0" }"#,
        );

    let output = run_in(proj.path(), &["left-pad"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "a library has nothing to run");
    assert!(stderr.contains("declares no binary"), "stderr: {stderr}");
}

#[test]
fn an_ambiguous_dependency_lists_the_binaries_to_choose_from() {
    let proj = TempProject::new("multi-bin")
        .file("package.json", r#"{ "dependencies": { "toolkit": "^1" } }"#)
        .file("package-lock.json", "{}")
        .file(
            "node_modules/toolkit/package.json",
            r#"{ "name": "toolkit", "bin": { "alpha": "./a.js", "beta": "./b.js" } }"#,
        );

    let output = run_in(proj.path(), &["toolkit"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "runner must not guess a binary");
    assert!(
        stderr.contains("alpha") && stderr.contains("beta"),
        "name the options. stderr: {stderr}",
    );
}
