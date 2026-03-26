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
