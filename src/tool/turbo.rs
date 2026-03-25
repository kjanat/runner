//! Turborepo — monorepo build system.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Directories produced by Turborepo.
pub const CLEAN_DIRS: &[&str] = &[".turbo"];

/// Detected via `turbo.json`.
pub fn detect(dir: &Path) -> bool {
    dir.join("turbo.json").exists()
}

/// Parse task names from `turbo.json`.
///
/// Supports both v2 (`"tasks"`) and v1 (`"pipeline"`) schemas. Scoped
/// tasks like `"my-app#build"` are filtered out.
pub fn extract_tasks(dir: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(dir.join("turbo.json")) else {
        return vec![];
    };
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
        pipeline: Option<HashMap<String, serde_json::Value>>,
    }
    let Ok(p) = serde_json::from_str::<Partial>(&content) else {
        return vec![];
    };
    let Some(tasks) = p.tasks.or(p.pipeline) else {
        return vec![];
    };
    tasks
        .into_keys()
        .filter(|name| !name.contains('#'))
        .collect()
}

/// `turbo run <task> [-- args...]`
pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("turbo");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}
