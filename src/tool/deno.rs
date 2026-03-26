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
    let Ok(d) = json5::from_str::<Partial>(&content) else {
        return vec![];
    };
    d.tasks.map_or(vec![], |t| t.into_keys().collect())
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

    use super::extract_tasks;
    use crate::tool::test_support::TempDir;

    #[test]
    fn extract_tasks_supports_jsonc_comments_and_trailing_commas() {
        let dir = TempDir::new("deno-jsonc");

        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{
  // line comment
  "tasks": {
    "build": "deno task build",
    /* block comment */
    "test": "deno test",
  },
}
"#,
        )
        .expect("deno.jsonc should be written");

        let mut tasks = extract_tasks(dir.path());
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "test"]);
    }
}
