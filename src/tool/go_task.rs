//! go-task — a task runner using YAML-based Taskfiles.
//!
//! Supports all [official filename variants](https://taskfile.dev/usage/#supported-file-names),
//! including `.dist` overrides.

use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

/// Priority order per the Taskfile specification.
pub(crate) const FILENAMES: &[&str] = &[
    "Taskfile.yml",
    "taskfile.yml",
    "Taskfile.yaml",
    "taskfile.yaml",
    "Taskfile.dist.yml",
    "taskfile.dist.yml",
    "Taskfile.dist.yaml",
    "taskfile.dist.yaml",
];

/// Detected via any supported Taskfile variant.
pub(crate) fn detect(dir: &Path) -> bool {
    files::find_first(dir, FILENAMES).is_some()
}

/// Extract task names with optional descriptions.
///
/// Prefers `task --list-all --json` when available for parity with go-task's
/// own file resolution behavior, then falls back to lightweight source parsing.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    if let Some(tasks) = extract_tasks_with_task(dir) {
        return Ok(tasks);
    }

    extract_tasks_from_source(dir)
}

fn extract_tasks_with_task(dir: &Path) -> Option<Vec<(String, Option<String>)>> {
    let output = Command::new("task")
        .arg("--list-all")
        .arg("--json")
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_task_list_json(&output.stdout)
}

fn parse_task_list_json(stdout: &[u8]) -> Option<Vec<(String, Option<String>)>> {
    #[derive(Deserialize)]
    struct ListOutput {
        tasks: Vec<ListTask>,
    }

    #[derive(Deserialize)]
    struct ListTask {
        name: String,
        #[serde(default)]
        desc: String,
    }

    let output = serde_json::from_slice::<ListOutput>(stdout).ok()?;
    Some(
        output
            .tasks
            .into_iter()
            .filter(|task| !task.name.is_empty())
            .map(|task| {
                let desc = (!task.desc.is_empty()).then_some(task.desc);
                (task.name, desc)
            })
            .collect(),
    )
}

fn extract_tasks_from_source(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
    let mut tasks: Vec<(String, Option<String>)> = Vec::new();
    let mut in_tasks = false;
    let mut task_indent: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim() == "tasks:" {
            in_tasks = true;
            task_indent = None;
            i += 1;
            continue;
        }
        if in_tasks {
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
                break;
            }
            let indent: String = line
                .chars()
                .take_while(|ch| *ch == ' ' || *ch == '\t')
                .collect();
            let stripped = task_indent
                .as_deref()
                .and_then(|expected_indent| line.strip_prefix(expected_indent))
                .or_else(|| (!indent.is_empty()).then_some(&line[indent.len()..]));
            if let Some(rest) = stripped
                && !rest.starts_with(' ')
                && !rest.starts_with('\t')
                && let Some(colon) = rest.find(':')
            {
                if task_indent.is_none() {
                    task_indent = Some(indent.clone());
                }
                let name = rest[..colon].trim();
                if !name.is_empty()
                    && !name.starts_with('#')
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    let desc = scan_desc(&lines, i + 1, &indent);
                    tasks.push((name.to_string(), desc));
                }
            }
        }
        i += 1;
    }
    Ok(tasks)
}

/// Scan lines after a task definition for a `desc:` field at deeper indentation.
fn scan_desc(lines: &[&str], start: usize, task_indent: &str) -> Option<String> {
    for line in &lines[start..] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(task_indent) || !line[task_indent.len()..].starts_with([' ', '\t']) {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("desc:") {
            let val = rest.trim().trim_matches(|c| c == '\'' || c == '"');
            return (!val.is_empty()).then(|| val.to_string());
        }
        if trimmed.contains(':') && !trimmed.starts_with('#') {
            break;
        }
    }
    None
}

/// `task <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("task");
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{extract_tasks, parse_task_list_json};
    use crate::tool::test_support::TempDir;

    #[test]
    fn parse_task_list_json_extracts_names_and_descriptions() {
        let tasks = parse_task_list_json(
            br#"{"tasks":[{"name":"default","desc":"Run default"},{"name":"test","desc":""}],"location":"/tmp/Taskfile.yml"}"#,
        )
        .expect("task --json output should parse");

        assert_eq!(
            tasks,
            [
                ("default".to_string(), Some("Run default".to_string())),
                ("test".to_string(), None),
            ]
        );
    }

    #[test]
    fn parse_task_list_json_ignores_empty_task_names() {
        let tasks = parse_task_list_json(
            br#"{"tasks":[{"name":""},{"name":"lint"}],"location":"/tmp/Taskfile.yml"}"#,
        )
        .expect("task --json output should parse");

        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["lint"]);
    }

    #[test]
    fn extract_tasks_supports_four_space_indentation() {
        let dir = TempDir::new("go-task-indent");

        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n    build:\n      cmds:\n        - cargo build\n    test:\n      cmds:\n        - cargo test\n",
        )
        .expect("Taskfile.yml should be written");

        let tasks = extract_tasks(dir.path()).expect("Taskfile tasks should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["build", "test"]);
    }

    #[test]
    fn extract_tasks_from_source_captures_desc() {
        let dir = TempDir::new("go-task-desc");

        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  build:\n    desc: Build the project\n    cmds:\n      - cargo build\n  lint:\n    cmds:\n      - cargo clippy\n",
        )
        .expect("Taskfile.yml should be written");

        let tasks =
            super::extract_tasks_from_source(dir.path()).expect("Taskfile tasks should parse");

        assert_eq!(
            tasks,
            [
                ("build".to_string(), Some("Build the project".to_string())),
                ("lint".to_string(), None),
            ]
        );
    }

    #[test]
    fn extract_tasks_supports_single_space_indentation() {
        let dir = TempDir::new("go-task-single-indent");

        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n build:\n  cmds:\n   - cargo build\n test:\n  cmds:\n   - cargo test\n",
        )
        .expect("Taskfile.yml should be written");

        let tasks = extract_tasks(dir.path()).expect("Taskfile tasks should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["build", "test"]);
    }
}
