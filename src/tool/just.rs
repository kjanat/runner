//! just — a handy command runner using `justfile`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

const FILENAMES: &[&str] = &["justfile", "Justfile", ".justfile"];

/// Detected via `justfile`, `Justfile`, or `.justfile`.
pub(crate) fn detect(dir: &Path) -> bool {
    FILENAMES.iter().any(|n| dir.join(n).exists())
}

/// Parse public recipe names from a justfile.
pub(crate) fn extract_tasks(dir: &Path) -> Vec<String> {
    let Some(path) = find_file(dir) else {
        return vec![];
    };

    extract_tasks_with_just(&path).unwrap_or_else(|| extract_tasks_from_source(&path))
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

fn extract_tasks_from_source(path: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
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
            if recipe[colon..].starts_with(":=") {
                continue;
            }
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

/// `just <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("just");
    c.arg(task).args(args);
    c
}

fn find_file(dir: &Path) -> Option<PathBuf> {
    FILENAMES.iter().map(|n| dir.join(n)).find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{extract_tasks, extract_tasks_from_source};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("runner-{prefix}-{id}"));
            fs::create_dir(&path).expect("temp dir should be created");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn fallback_parser_skips_private_and_directive_lines() {
        let dir = TempDir::new("just-fallback");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "set shell := [\"bash\", \"-cu\"]\nfoo := \"bar\"\n\nbuild:\n  echo build\n\n_secret:\n  echo nope\n\n@quiet name=\"world\":\n  echo hi {{name}}\n",
        )
        .expect("justfile should be written");

        assert_eq!(extract_tasks_from_source(&path), ["build", "quiet"]);
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

        assert_eq!(extract_tasks(dir.path()), ["build", "quiet"]);
    }
}
