//! Deno — secure JavaScript/TypeScript runtime.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

/// Directories produced by Deno.
pub(crate) const CLEAN_DIRS: &[&str] = &[".deno"];

/// Supported Deno config filenames (priority order).
pub(crate) const FILENAMES: &[&str] = &["deno.json", "deno.jsonc"];

/// Detected via `deno.json` or `deno.jsonc`.
pub(crate) fn detect(dir: &Path) -> bool {
    files::find_first(dir, FILENAMES).is_some()
}

/// Parse task names from `deno.json` / `deno.jsonc`.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let d = json5::from_str::<Partial>(&content)
        .with_context(|| format!("{} is not valid JSON/JSONC", path.display()))?;
    Ok(d.tasks.map_or_else(Vec::new, |t| t.into_keys().collect()))
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

        let mut tasks = extract_tasks(dir.path()).expect("deno tasks should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "test"]);
    }
}
