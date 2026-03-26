//! Deno — secure JavaScript/TypeScript runtime.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Directories produced by Deno.
pub(crate) const CLEAN_DIRS: &[&str] = &[".deno"];

/// Detected via `deno.json` or `deno.jsonc`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("deno.json").exists() || dir.join("deno.jsonc").exists()
}

/// Parse task names from `deno.json` / `deno.jsonc`.
pub(crate) fn extract_tasks(dir: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let path = if dir.join("deno.json").exists() {
        dir.join("deno.json")
    } else if dir.join("deno.jsonc").exists() {
        dir.join("deno.jsonc")
    } else {
        return vec![];
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let json = strip_jsonc_comments(&content);
    let Ok(d) = serde_json::from_str::<Partial>(&json) else {
        return vec![];
    };
    d.tasks.map_or(vec![], |t| t.into_keys().collect())
}

fn strip_jsonc_comments(content: &str) -> String {
    let mut stripped = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
                stripped.push(ch);
            }
            continue;
        }

        if block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                let _ = chars.next();
                block_comment = false;
            } else if ch == '\n' {
                stripped.push(ch);
            }
            continue;
        }

        if in_string {
            stripped.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                stripped.push(ch);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    let _ = chars.next();
                    line_comment = true;
                }
                Some('*') => {
                    let _ = chars.next();
                    block_comment = true;
                }
                _ => stripped.push(ch),
            },
            _ => stripped.push(ch),
        }
    }

    stripped
}

/// `deno task <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("task").arg(task).args(args);
    c
}

/// `deno install`
pub(crate) fn install_cmd() -> Command {
    let mut c = Command::new("deno");
    c.arg("install");
    c
}

/// `deno run <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("run").args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::extract_tasks;

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("runner-{prefix}-{id}"));
            fs::create_dir(&path).expect("temp dir should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn extract_tasks_supports_commented_jsonc() {
        let dir = TempDir::new("deno-jsonc");

        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{
  // line comment
  "tasks": {
    "build": "deno task build",
    /* block comment */
    "test": "deno test"
  }
}
"#,
        )
        .expect("deno.jsonc should be written");

        let mut tasks = extract_tasks(dir.path());
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "test"]);
    }
}
