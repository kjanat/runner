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

/// Parse Makefile targets, capturing `## Doc comment` lines preceding targets.
///
/// Extracts lines matching `target:` while skipping recipe lines (tab-
/// indented), special targets (`.PHONY` etc.), variable assignments (`:=`,
/// `:::=`), and pattern rules (`%`). A `## comment` immediately before a
/// target is returned as its description.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut targets = Vec::new();
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
        if !target.is_empty()
            && !target.contains(' ')
            && !target.contains('$')
            && !target.contains('%')
        {
            let doc = last_doc.take().filter(|d| !d.is_empty());
            targets.push((target.to_string(), doc));
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
    let mut c = Command::new("make");
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
    fn extract_tasks_captures_double_hash_comments() {
        let dir = TempDir::new("make-comments");
        fs::write(
            dir.path().join("Makefile"),
            "## Build the project\nbuild:\n\t@echo build\n\n## Run the test suite\ntest:\n\t@echo test\n\nclean:\n\t@echo clean\n",
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
