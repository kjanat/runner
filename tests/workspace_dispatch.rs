//! Integration coverage for workspace member tasks.
//!
//! Each test builds a throwaway npm workspace (`package.json` with
//! `"workspaces"`) whose members declare scripts, then drives the real
//! `runner` binary from the root, from inside a member, and from a plain
//! subdirectory: member scripts must list, resolve, and run in the member's
//! own directory, and the workspace must be visible from every directory
//! beneath its root.
//!
//! Dispatching a `package.json` script needs a Node package manager on
//! PATH; tests that spawn one skip with a note when `npm` is absent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Self-cleaning temp workspace.
struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "runner-workspace-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp workspace dir");
        Self { path }
    }

    fn file(self, name: &str, contents: &str) -> Self {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write workspace file");
        self
    }

    fn dir(self, name: &str) -> Self {
        std::fs::create_dir_all(self.path.join(name)).expect("create dir");
        self
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn runner_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runner"))
}

fn npm_available() -> bool {
    tool_available("npm")
}

fn make_available() -> bool {
    tool_available("make")
}

fn tool_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run `runner` against `dir` with every `RUNNER_*` variable scrubbed.
fn runner_in(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(runner_binary());
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RUNNER_")
        {
            cmd.env_remove(&key);
        }
    }
    cmd.arg("--dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("runner should execute")
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn json(output: &Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    serde_json::from_str(&stdout).expect("--json output parses")
}

const CWD_SCRIPT: &str = "console.log(process.cwd());\n";

/// Root (script `hello`) with an npm workspace of two members: `rfc`
/// (scripts `site` and `check`) and `@acme/web` at `apps/web` (script
/// `site`), plus a plain `docs/` directory and a `.git` marker so the
/// upward walk stops here. A `package-lock.json` pins npm as the PM.
fn workspace(tag: &str) -> TempWorkspace {
    TempWorkspace::new(tag)
        .dir(".git")
        .dir("docs")
        .file(
            "package.json",
            r#"{ "name": "root", "private": true, "workspaces": ["rfc", "apps/*"], "scripts": { "hello": "node cwd.js" } }"#,
        )
        .file("package-lock.json", "{}")
        .file("cwd.js", CWD_SCRIPT)
        .file("rfc/cwd.js", CWD_SCRIPT)
        .file("apps/web/cwd.js", CWD_SCRIPT)
        .file(
            "rfc/package.json",
            r#"{ "name": "rfc", "scripts": { "site": "node cwd.js", "check": "node cwd.js" } }"#,
        )
        .file(
            "apps/web/package.json",
            r#"{ "name": "@acme/web", "scripts": { "site": "node cwd.js" } }"#,
        )
}

const ROOT_MAKEFILE: &str = "build:\n\t@pwd\nlint:\n\t@pwd\n.PHONY: build lint\n";
const MEMBER_MAKEFILE: &str = "build:\n\t@pwd\nserve:\n\t@pwd\n.PHONY: build serve\n";

/// [`workspace`] with a `Makefile` at the root and another inside `rfc`.
fn workspace_with_makefiles(tag: &str) -> TempWorkspace {
    workspace(tag)
        .file("Makefile", ROOT_MAKEFILE)
        .file("rfc/Makefile", MEMBER_MAKEFILE)
}

#[test]
fn inside_a_member_its_makefile_is_read_and_the_roots_is_qualified() {
    let ws = workspace_with_makefiles("list-member-make");

    let output = runner_in(&ws.path().join("rfc"), &["list", "--raw"]);
    assert!(output.status.success());
    let lines = stdout_lines(&output);
    for expected in ["build", "serve", "root:build", "lint"] {
        assert!(
            lines.contains(&expected.to_string()),
            "{expected}: {lines:?}"
        );
    }
    assert!(!lines.contains(&"root:lint".to_string()), "{lines:?}");
}

#[test]
fn from_the_root_a_member_makefile_is_member_qualified() {
    let ws = workspace_with_makefiles("list-root-make");

    let output = runner_in(ws.path(), &["list", "--raw"]);
    assert!(output.status.success());
    let lines = stdout_lines(&output);
    for expected in ["build", "lint", "rfc:build", "rfc:serve"] {
        assert!(
            lines.contains(&expected.to_string()),
            "{expected}: {lines:?}"
        );
    }
}

#[test]
fn member_makefile_targets_run_in_the_member_directory() {
    if !make_available() {
        eprintln!("skipping: `make` not found on PATH");
        return;
    }
    let ws = workspace_with_makefiles("dispatch-make");
    let rfc = ws.path().join("rfc");

    assert_runs_in(&rfc, "make:build", &rfc);
    assert_runs_in(&rfc, "serve", &rfc);
    assert_runs_in(&rfc, "root:make:build", ws.path());
    assert_runs_in(&rfc, "make:lint", ws.path());
    assert_runs_in(ws.path(), "rfc:make:serve", &rfc);
    assert_runs_in(ws.path(), "make:build", ws.path());
}

#[test]
fn member_scripts_are_listed_from_the_root() {
    let ws = workspace("list");

    let output = runner_in(ws.path(), &["list", "--raw"]);
    assert!(output.status.success());
    assert_eq!(
        stdout_lines(&output),
        ["hello", "@acme/web:site", "rfc:check", "rfc:site"]
    );
}

#[test]
fn inside_a_member_its_scripts_are_bare_and_siblings_are_qualified() {
    let ws = workspace("list-member");

    let output = runner_in(&ws.path().join("rfc"), &["list", "--raw"]);
    assert!(output.status.success());
    assert_eq!(
        stdout_lines(&output),
        ["check", "site", "hello", "@acme/web:site"]
    );
}

#[test]
fn a_plain_subdirectory_sees_the_whole_workspace() {
    let ws = workspace("list-docs");

    let output = runner_in(&ws.path().join("docs"), &["list", "--raw"]);
    assert!(output.status.success());
    assert_eq!(
        stdout_lines(&output),
        ["hello", "@acme/web:site", "rfc:check", "rfc:site"]
    );
}

#[test]
fn list_json_names_the_member_of_each_task() {
    let ws = workspace("list-json");

    let json = json(&runner_in(ws.path(), &["list", "--json"]));
    let members: Vec<Option<&str>> = json["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .map(|task| task["member"].as_str())
        .collect();
    assert_eq!(members, [None, Some("@acme/web"), Some("rfc"), Some("rfc")]);
}

#[test]
fn doctor_reports_workspace_members_and_scoped_tasks() {
    let ws = workspace("doctor");

    let json = json(&runner_in(ws.path(), &["doctor", "--json"]));

    assert_eq!(json["project"]["monorepo"], true);
    assert_eq!(
        json["project"]["workspace"]["current"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["project"]["workspace"]["kinds"],
        serde_json::json!(["package.json workspaces"])
    );
    let paths: Vec<&str> = json["project"]["workspace"]["members"]
        .as_array()
        .expect("members array")
        .iter()
        .map(|member| member["path"].as_str().expect("path"))
        .collect();
    assert_eq!(paths, ["apps/web", "rfc"]);

    let fqns: Vec<&str> = json["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .map(|task| task["fqn"].as_str().expect("fqn"))
        .collect();
    assert_eq!(
        fqns,
        [
            "root:package.json#hello",
            "@acme/web:package.json#site",
            "rfc:package.json#check",
            "rfc:package.json#site",
        ]
    );
    let rfc_site = &json["tasks"][3];
    assert_eq!(rfc_site["scope"], "rfc");
    assert_eq!(rfc_site["cwd"], ws.path().join("rfc").display().to_string());

    let source_ids: Vec<&str> = json["sources"]
        .as_array()
        .expect("sources array")
        .iter()
        .map(|source| source["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        source_ids,
        [
            "src:root:package.json",
            "src:@acme/web:package.json",
            "src:rfc:package.json",
        ]
    );
    assert!(
        json["conflicts"].as_array().is_some_and(Vec::is_empty),
        "same-named tasks in different members are not a conflict: {}",
        json["conflicts"]
    );
}

#[test]
fn doctor_from_inside_a_member_anchors_on_the_workspace_root() {
    let ws = workspace("doctor-member");

    let json = json(&runner_in(&ws.path().join("rfc"), &["doctor", "--json"]));

    assert_eq!(json["project"]["root"], ws.path().display().to_string());
    assert_eq!(
        json["project"]["root_source"],
        "workspace root of member rfc"
    );
    assert_eq!(json["project"]["workspace"]["current"], "rfc");
}

#[test]
fn why_explains_a_bare_name_two_members_define() {
    let ws = workspace("why-ambiguous");

    let json = json(&runner_in(ws.path(), &["why", "site", "--json"]));
    assert_eq!(json["decision"]["strategy"], "ambiguous");
    assert_eq!(json["selected"], serde_json::Value::Null);
    assert_eq!(json["candidates"].as_array().map(Vec::len), Some(2));
}

#[test]
fn a_bare_name_two_members_define_is_refused_with_the_qualified_spellings() {
    let ws = workspace("ambiguous");

    let output = runner_in(ws.path(), &["run", "site"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "an ambiguous name must not run");
    assert!(stderr.contains("2 workspace members"), "stderr: {stderr}");
    assert!(
        stderr.contains("`rfc:site`") && stderr.contains("`@acme/web:site`"),
        "the error must spell out both qualified forms. stderr: {stderr}",
    );
}

#[test]
fn a_source_qualified_name_two_members_define_is_refused_too() {
    let ws = workspace("ambiguous-qualified");

    let output = runner_in(ws.path(), &["run", "package.json:site"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "a source qualifier pins the source, never the member"
    );
    assert!(stderr.contains("2 workspace members"), "stderr: {stderr}");
    assert!(
        stderr.contains("`rfc:site`") && stderr.contains("`@acme/web:site`"),
        "stderr: {stderr}",
    );
}

#[test]
fn an_unknown_member_prefix_is_reported_not_sent_to_npx() {
    let ws = workspace("unknown-member");

    let output = runner_in(ws.path(), &["run", "nope:package.json#site"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("no workspace member named \"nope\""),
        "stderr: {stderr}"
    );
}

/// Whether some stdout line names `expected` once both sides are canonical.
fn printed_dir(stdout: &str, expected: &Path) -> bool {
    stdout
        .lines()
        .any(|line| std::fs::canonicalize(line.trim()).is_ok_and(|dir| dir == expected))
}

/// Run `token` from `from` and assert the cwd-printing script printed
/// `expected`.
fn assert_runs_in(from: &Path, token: &str, expected: &Path) {
    let expected = std::fs::canonicalize(expected).expect("expected dir exists");
    let output = runner_in(from, &["run", token]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "{token} from {}: stdout: {stdout}\nstderr: {stderr}",
        from.display(),
    );
    assert!(
        printed_dir(&stdout, &expected),
        "{token} from {}: the script must run inside {}. stdout: {stdout}",
        from.display(),
        expected.display(),
    );
}

#[test]
fn member_scripts_run_in_the_member_directory() {
    if !npm_available() {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let ws = workspace("dispatch");
    let rfc = ws.path().join("rfc");

    for token in ["rfc:site", "rfc:package.json:site", "check"] {
        assert_runs_in(ws.path(), token, &rfc);
    }
}

#[test]
fn inside_a_member_the_member_wins_and_the_root_is_reachable() {
    if !npm_available() {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let ws = workspace("dispatch-member");
    let rfc = ws.path().join("rfc");
    let web = ws.path().join("apps/web");

    assert_runs_in(&rfc, "site", &rfc);
    assert_runs_in(&rfc, "hello", ws.path());
    assert_runs_in(&rfc, "@acme/web:site", &web);
    assert_runs_in(&rfc, "root:package.json#hello", ws.path());
}

#[test]
fn explain_reports_the_scope_a_member_task_was_picked_from() {
    if !npm_available() {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let ws = workspace("explain-scope");
    let rfc = ws.path().join("rfc");

    let output = runner_in(&rfc, &["--explain", "run", "site"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("scope: rfc (current member), outranks @acme/web"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!("dir={}", rfc.display())),
        "stderr: {stderr}"
    );

    let output = runner_in(ws.path(), &["--explain", "run", "hello"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("scope: root;"), "stderr: {stderr}");
}

#[test]
fn quiet_hides_the_member_dispatch_arrow_and_keeps_task_output() {
    if !npm_available() {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let ws = workspace("quiet-member");
    let rfc = std::fs::canonicalize(ws.path().join("rfc")).expect("member dir exists");

    let output = runner_in(ws.path(), &["-q", "run", "rfc:site"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !stderr.contains("→ package.json"),
        "-q must hide the dispatch arrow. stderr: {stderr}"
    );
    assert!(
        printed_dir(&stdout, &rfc),
        "task stdout survives -q. stdout: {stdout}"
    );
}

#[test]
fn per_task_config_addresses_a_member_task_by_member_and_name() {
    if !npm_available() {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let ws = workspace("member-task-config").file(
        "runner.toml",
        "[tasks.\"rfc:site\"]\nstdout = \"discard\"\n",
    );
    let web = std::fs::canonicalize(ws.path().join("apps/web")).expect("member dir exists");
    let rfc = std::fs::canonicalize(ws.path().join("rfc")).expect("member dir exists");

    let output = runner_in(ws.path(), &["run", "rfc:site"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        !printed_dir(&stdout, &rfc),
        "[tasks.\"rfc:site\"] stdout = \"discard\" must drop the member's stdout. stdout: {stdout}",
    );

    let output = runner_in(ws.path(), &["run", "@acme/web:site"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        printed_dir(&stdout, &web),
        "the entry is scoped to rfc; web keeps its stdout. stdout: {stdout}"
    );
}

#[test]
fn fully_qualified_tasks_work_from_any_directory() {
    if !npm_available() {
        eprintln!("skipping: `npm` not found on PATH");
        return;
    }
    let ws = workspace("dispatch-fqn");
    let rfc = ws.path().join("rfc");

    for from in [
        ws.path().to_path_buf(),
        rfc.clone(),
        ws.path().join("apps/web"),
        ws.path().join("docs"),
    ] {
        assert_runs_in(&from, "rfc:package.json#site", &rfc);
        assert_runs_in(&from, "root:package.json#hello", ws.path());
    }
}
