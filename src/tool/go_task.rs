use std::path::{Path, PathBuf};
use std::process::Command;

const FILENAMES: &[&str] = &["Taskfile.yml", "Taskfile.yaml", "taskfile.yml"];

pub fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

pub fn extract_tasks(dir: &Path) -> Vec<String> {
    let Some(content) = find_file(dir).and_then(|p| std::fs::read_to_string(p).ok()) else {
        return vec![];
    };
    let mut tasks = Vec::new();
    let mut in_tasks = false;
    for line in content.lines() {
        if line.trim() == "tasks:" {
            in_tasks = true;
            continue;
        }
        if in_tasks {
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
                break;
            }
            let stripped = line.strip_prefix("  ").or_else(|| line.strip_prefix('\t'));
            if let Some(rest) = stripped
                && !rest.starts_with(' ')
                && !rest.starts_with('\t')
                && let Some(colon) = rest.find(':')
            {
                let name = rest[..colon].trim();
                if !name.is_empty()
                    && !name.starts_with('#')
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    tasks.push(name.to_string());
                }
            }
        }
    }
    tasks
}

pub fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("task");
    c.arg(task).args(args);
    c
}

fn find_file(dir: &Path) -> Option<PathBuf> {
    FILENAMES.iter().map(|n| dir.join(n)).find(|p| p.exists())
}
