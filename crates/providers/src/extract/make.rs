//! GNU Make, build automation via `Makefile`.

use std::path::Path;

use anyhow::Context as _;

use super::files;

/// Configuration filenames in lookup order.
pub const FILENAMES: &[&str] = &["Makefile", "GNUmakefile", "makefile"];
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
pub fn detect(dir: &Path) -> bool {
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
pub fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
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

/// The first forwarded word make would not treat as a variable assignment.
///
/// GNU make has no recipe-argument passthrough: a word after the goal is
/// either one of make's own options or another goal. A variable assignment
/// is the one form that reaches the recipe, through `$(NAME)`.
pub fn first_non_assignment(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|arg| !is_assignment(arg))
}

/// GNU make's command-line assignment grammar: `NAME` followed by `=`,
/// `:=`, `::=`, `:::=`, `+=`, `?=` or `!=`, where the name is any run of
/// characters without whitespace, `:`, `#` or `=`.
fn is_assignment(arg: &str) -> bool {
    let Some((lhs, _)) = arg.split_once('=') else {
        return false;
    };
    let name = lhs
        .strip_suffix(['+', '?', '!'])
        .unwrap_or_else(|| lhs.trim_end_matches(':'));
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains([':', '#'])
        && !name.chars().any(char::is_whitespace)
}

/// Tasks declared by this provider in its observed scope.
///
/// # Errors
/// Returns a warning when the source or the provider query cannot be read.
pub fn tasks(
    present: &runner_core::Present,
    tree: &runner_core::Tree,
) -> Result<Vec<runner_core::Task>, runner_core::Warning> {
    let root = runner_core::plan::scope_dir(tree, &present.scope);
    let extracted = extract_tasks(&root)
        .map_err(|e| runner_core::Warning::about(present.provider, e.to_string()))?;
    Ok(extracted
        .into_iter()
        .map(|(name, description)| super::task(present, name, description))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::test_support::TempDir;
    use std::fs;
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

#[cfg(test)]
mod verbosity_tests {

    #[test]
    fn variable_assignments_pass_and_anything_else_is_named() {
        use super::first_non_assignment;
        let ok = [String::from("CC=clang"), String::from("ARGS=-run TestFoo")];
        assert_eq!(first_non_assignment(&ok), None);
        let flag = [String::from("CC=clang"), String::from("--help")];
        assert_eq!(first_non_assignment(&flag), Some("--help"));
        let goal = [String::from("clean")];
        assert_eq!(first_non_assignment(&goal), Some("clean"));
        let odd = [String::from("1X=y"), String::from("=y")];
        assert_eq!(first_non_assignment(&odd), Some("=y"));
        let forms = [
            String::from("CFLAGS+=-g"),
            String::from("CC:=clang"),
            String::from("V::=1"),
            String::from("W:::=1"),
            String::from("DEBUG?=1"),
            String::from("REV!=git rev-parse HEAD"),
            String::from("foo-bar=1"),
        ];
        assert_eq!(first_non_assignment(&forms), None);
        let option = [String::from("-j=4")];
        assert_eq!(first_non_assignment(&option), Some("-j=4"));
        let spaced = [String::from("A B=1")];
        assert_eq!(first_non_assignment(&spaced), Some("A B=1"));
    }
}
