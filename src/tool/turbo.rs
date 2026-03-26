//! Turborepo — monorepo build system.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

/// Directories produced by Turborepo.
pub(crate) const CLEAN_DIRS: &[&str] = &[".turbo"];

/// Detected via `turbo.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("turbo.json").exists()
}

/// Parse task names from `turbo.json`.
///
/// Supports both v2 (`"tasks"`) and v1 (`"pipeline"`) schemas. Scoped
/// tasks like `"my-app#build"` are filtered out.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
        pipeline: Option<HashMap<String, serde_json::Value>>,
    }
    let path = dir.join("turbo.json");
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let p = serde_json::from_str::<Partial>(&content)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let Some(tasks) = p.tasks.or(p.pipeline) else {
        return Ok(vec![]);
    };
    Ok(tasks
        .into_keys()
        .filter(|name| !name.contains('#'))
        .collect())
}

/// `turbo run <task> [-- args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("turbo");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::extract_tasks;
    use crate::tool::test_support::TempDir;

    #[test]
    fn extract_tasks_returns_empty_when_turbo_json_is_missing() {
        let dir = TempDir::new("turbo-missing");

        assert!(
            extract_tasks(dir.path())
                .expect("missing turbo.json should be ok")
                .is_empty()
        );
    }

    #[test]
    fn extract_tasks_errors_on_malformed_json() {
        let dir = TempDir::new("turbo-malformed");
        fs::write(dir.path().join("turbo.json"), "{").expect("turbo.json should be written");

        assert!(extract_tasks(dir.path()).is_err());
    }

    #[test]
    fn extract_tasks_returns_empty_when_no_task_table_exists() {
        let dir = TempDir::new("turbo-empty");
        fs::write(dir.path().join("turbo.json"), "{}").expect("turbo.json should be written");

        assert!(
            extract_tasks(dir.path())
                .expect("empty turbo config should parse")
                .is_empty()
        );
    }

    #[test]
    fn extract_tasks_reads_v2_tasks_schema() {
        let dir = TempDir::new("turbo-v2");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"build":{},"lint":{},"web#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let mut tasks = extract_tasks(dir.path()).expect("v2 turbo config should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "lint"]);
    }

    #[test]
    fn extract_tasks_reads_v1_pipeline_schema() {
        let dir = TempDir::new("turbo-v1");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"pipeline":{"test":{},"typecheck":{},"pkg#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let mut tasks = extract_tasks(dir.path()).expect("v1 turbo config should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["test", "typecheck"]);
    }
}
