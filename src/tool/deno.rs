use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

pub const CLEAN_DIRS: &[&str] = &[".deno"];

pub fn detect(dir: &Path) -> bool {
    dir.join("deno.json").exists() || dir.join("deno.jsonc").exists()
}

pub fn extract_tasks(dir: &Path) -> Vec<String> {
    let path = if dir.join("deno.json").exists() {
        dir.join("deno.json")
    } else if dir.join("deno.jsonc").exists() {
        dir.join("deno.jsonc")
    } else {
        return vec![];
    };
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    // deno.jsonc may have comments — serde_json will fail, which is fine
    let Ok(d) = serde_json::from_str::<Partial>(&content) else {
        return vec![];
    };
    d.tasks.map_or(vec![], |t| t.into_keys().collect())
}

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("task").arg(task).args(args);
    c
}

pub fn install_cmd() -> Command {
    let mut c = Command::new("deno");
    c.arg("install");
    c
}

pub fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("run").args(args);
    c
}
