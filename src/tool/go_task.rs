//! go-task, a task runner using YAML-based Taskfiles.
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
    let output = super::program::command("task")
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

/// Fallback used when the `task` binary is absent: parse the Taskfile as
/// real YAML instead of line-scanning it. The previous hand-rolled
/// scanner silently dropped legal names its `[alnum]-_` filter didn't
/// recognize (quoted or namespaced keys like `"build:prod"`) and never
/// surfaced malformed YAML; a broken Taskfile just yielded zero tasks
/// with no `TaskListUnreadable` warning. Invalid YAML now errors so
/// detection can warn.
fn extract_tasks_from_source(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let docs = yaml_rust2::YamlLoader::load_from_str(&content)
        .with_context(|| format!("{} is not valid YAML", path.display()))?;
    let tasks_value = docs
        .first()
        .and_then(yaml_rust2::Yaml::as_hash)
        .and_then(|root| {
            root.iter()
                .find_map(|(key, value)| (key.as_str() == Some("tasks")).then_some(value))
        });
    let Some(tasks_value) = tasks_value else {
        // No `tasks:` table, legal for pure-include/vars Taskfiles.
        return Ok(vec![]);
    };
    // Present but not a mapping (`tasks: []`, `tasks: "x"`) is a broken
    // Taskfile, not an empty one; error so detection warns.
    let Some(tasks) = tasks_value.as_hash() else {
        anyhow::bail!(
            "{} has a `tasks` key, but its value is not a mapping",
            path.display(),
        );
    };
    Ok(tasks
        .iter()
        .filter_map(|(name, body)| {
            let name = name.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let desc = body
                .as_hash()
                .and_then(|fields| {
                    fields
                        .iter()
                        .find_map(|(key, value)| (key.as_str() == Some("desc")).then_some(value))
                })
                .and_then(yaml_rust2::Yaml::as_str)
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(ToOwned::to_owned);
            Some((name.to_owned(), desc))
        })
        .collect())
}

/// `task <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("task");
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{extract_tasks, extract_tasks_from_source, parse_task_list_json};
    use crate::tool::test_support::TempDir;

    #[test]
    fn source_fallback_keeps_quoted_and_namespaced_task_names() {
        // `"build:prod":` is a legal Taskfile key; the old line scanner's
        // `[alnum]-_` name filter silently dropped it.
        let dir = TempDir::new("go-task-quoted-names");
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\ntasks:\n  \"build:prod\":\n    desc: Production build\n    cmds:\n      \
             - go build\n  lint:\n    cmds:\n      - golangci-lint run\n",
        )
        .expect("Taskfile should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("Taskfile should parse");

        assert_eq!(
            tasks,
            [
                (
                    "build:prod".to_string(),
                    Some("Production build".to_string())
                ),
                ("lint".to_string(), None),
            ]
        );
    }

    #[test]
    fn source_fallback_errors_on_invalid_yaml() {
        // A broken Taskfile must surface as an error (→ TaskListUnreadable
        // warning), not silently yield zero tasks.
        let dir = TempDir::new("go-task-broken-yaml");
        fs::write(
            dir.path().join("Taskfile.yml"),
            "tasks:\n  build: [unclosed\n",
        )
        .expect("Taskfile should be written");

        let err = extract_tasks_from_source(dir.path()).expect_err("invalid YAML should error");
        assert!(format!("{err:#}").contains("not valid YAML"));
    }

    #[test]
    fn source_fallback_errors_when_tasks_is_not_a_mapping() {
        // `tasks: []` is YAML-valid but broken as a Taskfile, distinct
        // from having no `tasks:` key at all; it must error, not yield
        // zero tasks silently.
        let dir = TempDir::new("go-task-tasks-not-mapping");
        fs::write(dir.path().join("Taskfile.yml"), "version: '3'\ntasks: []\n")
            .expect("Taskfile should be written");

        let err = extract_tasks_from_source(dir.path())
            .expect_err("non-mapping tasks value should error");
        assert!(format!("{err:#}").contains("not a mapping"));
    }

    #[test]
    fn source_fallback_allows_taskfile_without_tasks_table() {
        // Pure include/vars Taskfiles have no `tasks:`, zero tasks, no error.
        let dir = TempDir::new("go-task-includes-only");
        fs::write(
            dir.path().join("Taskfile.yml"),
            "version: '3'\nincludes:\n  sub: ./sub/Taskfile.yml\n",
        )
        .expect("Taskfile should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("include-only Taskfile is fine");
        assert!(tasks.is_empty());
    }

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
            "version: '3'\ntasks:\n  build:\n    desc: Build the project\n    cmds:\n      - \
             cargo build\n  lint:\n    cmds:\n      - cargo clippy\n",
        )
        .expect("Taskfile.yml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("Taskfile tasks should parse");

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
            "version: '3'\ntasks:\n build:\n  cmds:\n   - cargo build\n test:\n  cmds:\n   - \
             cargo test\n",
        )
        .expect("Taskfile.yml should be written");

        let tasks = extract_tasks(dir.path()).expect("Taskfile tasks should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["build", "test"]);
    }
}
