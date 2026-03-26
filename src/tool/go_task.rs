//! go-task — a task runner using YAML-based Taskfiles.
//!
//! Supports all [official filename variants](https://taskfile.dev/usage/#supported-file-names),
//! including `.dist` overrides.

use std::path::Path;
use std::process::Command;

use anyhow::Context as _;

use crate::tool::files;

/// Priority order per the Taskfile specification.
const FILENAMES: &[&str] = &[
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
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

/// Lightweight YAML extraction: finds the `tasks:` block and collects
/// immediate child keys with the first detected task indentation.
///
/// Does not use a full YAML parser — relies on consistent indentation.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut tasks = Vec::new();
    let mut in_tasks = false;
    let mut task_indent: Option<String> = None;
    for line in content.lines() {
        if line.trim() == "tasks:" {
            in_tasks = true;
            task_indent = None;
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
                .or_else(|| {
                    (((indent.chars().filter(|ch| *ch == ' ').count() >= 2)
                        || indent.contains('\t'))
                        && !indent.is_empty())
                    .then_some(&line[indent.len()..])
                });
            if let Some(rest) = stripped
                && !rest.starts_with(' ')
                && !rest.starts_with('\t')
                && let Some(colon) = rest.find(':')
            {
                if task_indent.is_none() {
                    task_indent = Some(indent);
                }
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
    Ok(tasks)
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

    use super::extract_tasks;
    use crate::tool::test_support::TempDir;

    #[test]
    fn extract_tasks_supports_four_space_indentation() {
        let dir = TempDir::new("go-task-indent");

        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n    build:\n      cmds:\n        - cargo build\n    test:\n      cmds:\n        - cargo test\n",
        )
        .expect("Taskfile.yml should be written");

        assert_eq!(
            extract_tasks(dir.path()).expect("Taskfile tasks should parse"),
            ["build", "test"]
        );
    }
}
