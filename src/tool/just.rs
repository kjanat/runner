//! just — a handy command runner using `justfile`.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

const FILENAMES: &[&str] = &["justfile", "Justfile", ".justfile"];

/// Detected via `justfile`, `Justfile`, or `.justfile`.
pub(crate) fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

/// Parse public recipe names from a justfile.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };

    extract_tasks_with_just(&path).map_or_else(|| extract_tasks_from_source(&path), Ok)
}

fn extract_tasks_with_just(path: &Path) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    struct Dump {
        recipes: HashMap<String, Recipe>,
    }

    #[derive(Deserialize)]
    struct Recipe {
        private: bool,
    }

    let output = Command::new("just")
        .arg("--justfile")
        .arg(path)
        .arg("--dump-format")
        .arg("json")
        .arg("--dump")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dump = serde_json::from_slice::<Dump>(&output.stdout).ok()?;
    let mut recipes: Vec<String> = dump
        .recipes
        .into_iter()
        .filter_map(|(name, recipe)| (!recipe.private).then_some(name))
        .collect();
    recipes.sort_unstable();
    Some(recipes)
}

fn extract_tasks_from_source(path: &Path) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut recipes = Vec::new();
    let mut saw_private_attr = false;
    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            saw_private_attr |= is_private_attr(trimmed);
            continue;
        }
        if trimmed.starts_with("set ")
            || trimmed.starts_with("alias ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("include ")
            || trimmed.starts_with("mod ")
            || trimmed.starts_with("export ")
        {
            saw_private_attr = false;
            continue;
        }
        let recipe = trimmed.strip_prefix('@').unwrap_or(trimmed);
        if let Some(colon) = recipe.find(':') {
            if recipe[colon..].starts_with(":=") {
                saw_private_attr = false;
                continue;
            }
            let before = &recipe[..colon];
            let name = before.split_whitespace().next().unwrap_or("");
            if !name.is_empty()
                && !name.starts_with('_')
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                && !saw_private_attr
            {
                recipes.push(name.to_string());
            }
        }
        saw_private_attr = false;
    }
    Ok(recipes)
}

fn is_private_attr(trimmed: &str) -> bool {
    trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|attr| {
            attr.split(',')
                .map(str::trim)
                .any(|segment| segment.starts_with("private"))
        })
}

/// `just <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("just");
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::{extract_tasks, extract_tasks_from_source, is_private_attr};
    use crate::tool::test_support::TempDir;

    #[test]
    fn fallback_parser_skips_private_and_directive_lines() {
        let dir = TempDir::new("just-fallback");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "set shell := [\"bash\", \"-cu\"]\ninclude \"common.just\"\n[private]\nfoo := \"bar\"\n\n[private]\nsecret:\n  echo nope\n\nbuild:\n  echo build\n\n_secret:\n  echo nope\n\n@quiet name=\"world\":\n  echo hi {{name}}\n",
        )
        .expect("justfile should be written");

        assert_eq!(
            extract_tasks_from_source(&path).expect("justfile source should parse"),
            ["build", "quiet"]
        );
    }

    #[test]
    fn private_attr_matches_comma_separated_lists() {
        assert!(is_private_attr("[unix, private]"));
        assert!(is_private_attr("[private(no-cd), unix]"));
        assert!(!is_private_attr("[unix, linux]"));
    }

    #[test]
    fn extract_tasks_uses_just_json_when_available() {
        if Command::new("just").arg("--version").output().is_err() {
            return;
        }

        let dir = TempDir::new("just-json");
        fs::write(
            dir.path().join("justfile"),
            "build:\n  echo build\n\n_secret:\n  echo nope\n\n@quiet:\n  echo hi\n",
        )
        .expect("justfile should be written");

        assert_eq!(
            extract_tasks(dir.path()).expect("justfile tasks should parse"),
            ["build", "quiet"]
        );
    }
}
