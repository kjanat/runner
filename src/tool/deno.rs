//! Deno — secure JavaScript/TypeScript runtime.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Directories produced by Deno.
pub const CLEAN_DIRS: &[&str] = &[".deno"];

/// Detected via `deno.json` or `deno.jsonc`.
pub fn detect(dir: &Path) -> bool {
    dir.join("deno.json").exists() || dir.join("deno.jsonc").exists()
}

/// Parse task names from `deno.json` / `deno.jsonc`.
///
/// Note: `.jsonc` files with comments will fail `serde_json` parsing and
/// return an empty list. A JSONC-aware parser would be needed for full
/// support.
pub fn extract_tasks(dir: &Path) -> Vec<String> {
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
    let Ok(d) = serde_json::from_str::<Partial>(&content) else {
        return vec![];
    };
    d.tasks.map_or(vec![], |t| t.into_keys().collect())
}

/// `deno task <task> [args...]`
pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("task").arg(task).args(args);
    c
}

/// `deno install`
pub fn install_cmd() -> Command {
    let mut c = Command::new("deno");
    c.arg("install");
    c
}

/// `deno run <args...>`
pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("run").args(args);
    c
}
