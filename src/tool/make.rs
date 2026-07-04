//! GNU Make — build automation via `Makefile`.

use std::path::Path;
use std::process::Command;

use anyhow::Context as _;

use crate::tool::files;

pub(crate) const FILENAMES: &[&str] = &["Makefile", "GNUmakefile", "makefile"];
const SPECIAL_TARGETS: &[&str] = &[
    ".PHONY",
    ".SUFFIXES",
    ".DEFAULT",
    ".PRECIOUS",
    ".INTERMEDIATE",
    ".NOTINTERMEDIATE",
    ".SECONDARY",
    ".SECONDEXPANSION",
    ".DELETE_ON_ERROR",
    ".SILENT",
    ".IGNORE",
    ".LOW_RESOLUTION_TIME",
    ".EXPORT_ALL_VARIABLES",
    ".NOTPARALLEL",
    ".ONESHELL",
    ".POSIX",
];

/// Detected via `Makefile`, `GNUmakefile`, or `makefile`.
pub(crate) fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

/// Parse Makefile targets, capturing `## Doc comment` descriptions.
///
/// Extracts lines matching `target:` while skipping recipe lines (tab-
/// indented), special targets (`.PHONY` etc.), variable assignments (`:=`,
/// `:::=`), and pattern rules (`%`). Both self-documenting idioms are
/// supported: a `## comment` line immediately before a target, and the
/// inline `target: deps ## comment` form (the one `grep -E '.*?## '`
/// help targets are built on); the preceding-line form wins when both
/// are present. A target header appearing twice (legal in make) yields
/// one row; a later duplicate can still contribute the description if
/// the first occurrence had none.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut targets: Vec<(String, Option<String>)> = Vec::new();
    let mut index_by_name: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut last_doc: Option<String> = None;
    for line in content.lines() {
        if let Some(comment) = line.strip_prefix("##") {
            last_doc = Some(comment.trim().to_string());
            continue;
        }
        if line.starts_with('\t') || line.starts_with(' ') || line.starts_with('#') {
            last_doc = None;
            continue;
        }
        let Some(colon) = line.find(':') else {
            last_doc = None;
            continue;
        };
        let after = &line[colon..];
        if after.starts_with("::=") || after.starts_with(":=") || after.starts_with(":::=") {
            last_doc = None;
            continue;
        }
        let target = line[..colon].trim();
        if SPECIAL_TARGETS.contains(&target) || is_suffix_rule(target) {
            last_doc = None;
            continue;
        }
        let names: Vec<&str> = target
            .split_whitespace()
            .filter(|name| !name.is_empty() && !name.contains('$') && !name.contains('%'))
            .collect();
        if !names.is_empty() {
            let inline_doc = after
                .find("##")
                .map(|at| after[at + 2..].trim().to_string())
                .filter(|doc| !doc.is_empty());
            let doc = last_doc.take().filter(|d| !d.is_empty()).or(inline_doc);
            for name in names {
                if let Some(&at) = index_by_name.get(name) {
                    if targets[at].1.is_none() {
                        targets[at].1.clone_from(&doc);
                    }
                } else {
                    index_by_name.insert(name.to_string(), targets.len());
                    targets.push((name.to_string(), doc.clone()));
                }
            }
        }
        last_doc = None;
    }
    Ok(targets)
}

fn is_suffix_rule(target: &str) -> bool {
    target.starts_with('.') && target[1..].contains('.')
}

/// `make <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("make");
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::extract_tasks;
    use crate::tool::test_support::TempDir;

    #[test]
    fn extract_tasks_keeps_double_colon_rules() {
        let dir = TempDir::new("make-double-colon");
        fs::write(
            dir.path().join("Makefile"),
            "build::\n\t@echo first\nvalue :::= thing\n",
        )
        .expect("Makefile should be written");

        let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_tasks_splits_multi_target_rules() {
        let dir = TempDir::new("make-multi-target");
        fs::write(
            dir.path().join("Makefile"),
            "## shared docs\nbuild test:\n\t@echo ok\n",
        )
        .expect("Makefile should be written");

        let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        let docs: Vec<Option<&str>> = tasks.iter().map(|(_, d)| d.as_deref()).collect();

        assert_eq!(names, ["build", "test"]);
        assert_eq!(docs, [Some("shared docs"), Some("shared docs")]);
    }

    #[test]
    fn extract_tasks_keeps_dot_prefixed_targets() {
        let dir = TempDir::new("make-dot-target");
        fs::write(
            dir.path().join("Makefile"),
            ".PHONY: build\n.DELETE_ON_ERROR:\n.NOTPARALLEL:\n.c.o:\n.dev:\n\t@echo hi\n",
        )
        .expect("Makefile should be written");

        let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [".dev"]);
    }

    #[test]
    fn extract_tasks_captures_inline_double_hash_comments() {
        // The dominant self-documenting idiom puts the doc on the target
        // line itself: `build: ## Build the project`. Preceding-line form
        // wins when both are present.
        let dir = TempDir::new("make-inline-comments");
        fs::write(
            dir.path().join("Makefile"),
            "build: deps ## Build the project\n\t@echo build\n## Preceding wins\ntest: ## Inline \
             loses\n\t@echo test\nclean:\n\t@echo clean\n",
        )
        .expect("Makefile should be written");

        let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");

        assert_eq!(
            tasks,
            [
                ("build".to_string(), Some("Build the project".to_string())),
                ("test".to_string(), Some("Preceding wins".to_string())),
                ("clean".to_string(), None),
            ]
        );
    }

    #[test]
    fn extract_tasks_dedups_repeated_target_headers() {
        // A target header may legally appear twice (e.g. conditional
        // includes appending recipes); list it once, and let a later
        // documented occurrence fill in a missing description.
        let dir = TempDir::new("make-duplicate-targets");
        fs::write(
            dir.path().join("Makefile"),
            "build:\n\t@echo one\nbuild: ## Build the project\n\t@echo two\n",
        )
        .expect("Makefile should be written");

        let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");

        assert_eq!(
            tasks,
            [("build".to_string(), Some("Build the project".to_string()))]
        );
    }

    #[test]
    fn extract_tasks_captures_double_hash_comments() {
        let dir = TempDir::new("make-comments");
        fs::write(
            dir.path().join("Makefile"),
            "## Build the project\nbuild:\n\t@echo build\n\n## Run the test suite\ntest:\n\t@echo \
             test\n\nclean:\n\t@echo clean\n",
        )
        .expect("Makefile should be written");

        let tasks = extract_tasks(dir.path()).expect("Makefile targets should parse");

        assert_eq!(
            tasks,
            [
                ("build".to_string(), Some("Build the project".to_string())),
                ("test".to_string(), Some("Run the test suite".to_string())),
                ("clean".to_string(), None),
            ]
        );
    }
}
