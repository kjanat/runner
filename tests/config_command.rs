//! `runner config` against every config file spelling discovery accepts.

mod support;

use std::path::{Path, PathBuf};
use std::process::Output;

const SPELLINGS: [&str; 4] = [
    "runner.toml",
    ".runner.toml",
    ".config/runner.toml",
    ".config/.runner.toml",
];

const BODY: &str = "[chain]\non_fail = \"continue\"\n";

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "runner-config-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(".git")).expect("create temp root");
        Self { path }
    }

    fn with_config(tag: &str, spelling: &str) -> (Self, PathBuf) {
        let root = Self::new(tag);
        let file = root.path.join(spelling);
        std::fs::create_dir_all(file.parent().expect("config parent")).expect("create parent");
        std::fs::write(&file, BODY).expect("write config");
        (root, file)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn runner(dir: &Path, args: &[&str]) -> Output {
    support::command(env!("CARGO_BIN_EXE_runner"))
        .env("NO_COLOR", "1")
        .current_dir(dir)
        .args(["config"])
        .args(args)
        .output()
        .expect("runner should execute")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn path_prints_the_loaded_file() {
    for spelling in SPELLINGS {
        let (root, file) = TempRoot::with_config("path", spelling);
        let output = runner(root.path(), &["path"]);
        assert!(output.status.success(), "{spelling}: {}", stderr(&output));
        assert_eq!(
            stdout(&output).trim(),
            file.display().to_string(),
            "{spelling}"
        );
    }
}

#[test]
fn path_falls_back_to_the_plain_file_in_the_root() {
    let root = TempRoot::new("path-absent");
    let output = runner(root.path(), &["path"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output).trim(),
        root.path().join("runner.toml").display().to_string()
    );
}

#[test]
fn show_names_the_loaded_file() {
    for spelling in SPELLINGS {
        let (root, file) = TempRoot::with_config("show", spelling);
        let output = runner(root.path(), &["show"]);
        assert!(output.status.success(), "{spelling}: {}", stderr(&output));
        let out = stdout(&output);
        assert_eq!(
            out.lines().next(),
            Some(format!("config: {}", file.display()).as_str()),
            "{spelling}: {out}"
        );
    }
}

#[test]
fn show_json_reports_the_loaded_file() {
    for spelling in SPELLINGS {
        let (root, file) = TempRoot::with_config("show-json", spelling);
        let output = runner(root.path(), &["show", "--json"]);
        assert!(output.status.success(), "{spelling}: {}", stderr(&output));
        let report: serde_json::Value =
            serde_json::from_str(&stdout(&output)).expect("--json output parses");
        assert_eq!(
            report["path"],
            file.display().to_string(),
            "{spelling}: {report}"
        );
        assert_eq!(report["exists"], true, "{spelling}: {report}");
    }
}

#[test]
fn validate_names_the_loaded_file() {
    for spelling in SPELLINGS {
        let (root, file) = TempRoot::with_config("validate", spelling);
        let output = runner(root.path(), &["validate"]);
        assert!(output.status.success(), "{spelling}: {}", stderr(&output));
        assert_eq!(
            stdout(&output).trim(),
            format!("ok: {} is valid", file.display()),
            "{spelling}"
        );
    }
}

#[test]
fn init_refuses_when_any_config_file_exists() {
    for spelling in SPELLINGS {
        let (root, file) = TempRoot::with_config("init", spelling);
        let output = runner(root.path(), &["init"]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{spelling}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains(&format!("{} already exists", file.display())),
            "{spelling}: {}",
            stderr(&output)
        );
        assert_eq!(std::fs::read_to_string(&file).expect("read back"), BODY);
        if spelling != "runner.toml" {
            assert!(
                !root.path().join("runner.toml").exists(),
                "{spelling}: init wrote a shadowing runner.toml"
            );
        }
    }
}

#[test]
fn forced_init_overwrites_the_loaded_file() {
    for spelling in SPELLINGS {
        let (root, file) = TempRoot::with_config("init-force", spelling);
        let output = runner(root.path(), &["init", "--force"]);
        assert!(output.status.success(), "{spelling}: {}", stderr(&output));
        let written = std::fs::read_to_string(&file).expect("read back");
        assert!(written.starts_with("#:schema "), "{spelling}: {written}");
        if spelling != "runner.toml" {
            assert!(
                !root.path().join("runner.toml").exists(),
                "{spelling}: init wrote a shadowing runner.toml"
            );
        }
    }
}
