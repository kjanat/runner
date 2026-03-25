use std::path::{Path, PathBuf};
use std::process::Command;

const FILENAMES: &[&str] = &["justfile", "Justfile", ".justfile"];

pub fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

pub fn extract_tasks(dir: &Path) -> Vec<String> {
    let Some(content) = find_file(dir).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return vec![];
    };
    let mut recipes = Vec::new();
    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("set ")
            || trimmed.starts_with("alias ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("mod ")
            || trimmed.starts_with("export ")
        {
            continue;
        }
        let recipe = trimmed.strip_prefix('@').unwrap_or(trimmed);
        if let Some(colon) = recipe.find(':') {
            let before = &recipe[..colon];
            let name = before.split_whitespace().next().unwrap_or("");
            if !name.is_empty()
                && !name.starts_with('_')
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                recipes.push(name.to_string());
            }
        }
    }
    recipes
}

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("just");
    c.arg(task).args(args);
    c
}

fn find_file(dir: &Path) -> Option<PathBuf> {
    FILENAMES.iter().map(|n| dir.join(n)).find(|p| p.exists())
}
