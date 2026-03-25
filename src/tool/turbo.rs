use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

pub const CLEAN_DIRS: &[&str] = &[".turbo"];

pub fn detect(dir: &Path) -> bool {
    dir.join("turbo.json").exists()
}

pub fn extract_tasks(dir: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(dir.join("turbo.json")) else {
        return vec![];
    };
    // turbo v2 uses "tasks", v1 used "pipeline"
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

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("turbo");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}
