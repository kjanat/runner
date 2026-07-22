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

/// Run the `run` binary against `dir` with `RUNNER_*` scrubbed and the
/// binary's own directory on `PATH`, so a nested `run` reaches this build.
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
    assert!(
        String::from_utf8_lossy(&plain.stdout).contains("NODE"),
        "control should report NODE. stdout: {}",
        String::from_utf8_lossy(&plain.stdout),
    );

    // #94: `bun run` alone would still hand that `node` to system Node;
    // `bun --bun run` is what moves it.
    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "which"]);
    assert!(
        String::from_utf8_lossy(&forced.stdout).contains("BUN"),
        "--runtime bun must put the script's node on bun. stdout: {}",
        String::from_utf8_lossy(&forced.stdout),
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

    assert!(
        String::from_utf8_lossy(&output.stdout).contains("BUN"),
        "nested dispatch must inherit the runtime. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
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
    assert!(
        String::from_utf8_lossy(&plain.stdout).contains("NODE"),
        "control should report NODE. stdout: {}",
        String::from_utf8_lossy(&plain.stdout),
    );

    let forced = run_in(proj.path(), &["-q", "--runtime", "bun", "probe.js"]);
    assert!(
        String::from_utf8_lossy(&forced.stdout).contains("BUN"),
        "--runtime bun must run the file on bun. stdout: {}",
        String::from_utf8_lossy(&forced.stdout),
    );
}

#[test]
fn config_layer_sets_the_runtime() {
    if !tool_available("bun") || !tool_available("node") {
        eprintln!("skipping: bun or node not found on PATH");
        return;
    }
    let proj = probe_project("config").file("runner.toml", "[runtime]\njs = \"bun\"\n");
    let output = run_in(proj.path(), &["-q", "which"]);

    assert!(
        String::from_utf8_lossy(&output.stdout).contains("BUN"),
        "[runtime].js must apply without a flag. stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );
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
