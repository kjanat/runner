//! bacon — a Rust background checker driven by `bacon.toml`.
//!
//! Surfaces every `[jobs.<name>]` table as a runnable task. Bacon ships
//! with built-in jobs (`check`, `clippy`, `test`, …) baked into the binary
//! even when no `bacon.toml` exists; we deliberately don't enumerate those
//! since we can't read them without invoking bacon, and surfacing tasks
//! the user never declared would be misleading.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

pub(crate) const FILENAMES: &[&str] = &["bacon.toml"];

/// Detected via `bacon.toml`.
pub(crate) fn detect(dir: &Path) -> bool {
    files::find_first(dir, FILENAMES).is_some()
}

/// Extract job names with optional descriptions, sorted alphabetically.
///
/// Bacon's stock schema doesn't define a description field, but we accept
/// `desc` (and `description` as an alias) defensively so anything users
/// stuck in there round-trips into `runner list`. Jobs whose names start
/// with `_` are treated as private and hidden, mirroring the just-style
/// convention.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = files::find_first(dir, FILENAMES) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: BaconDoc =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    let mut tasks: Vec<(String, Option<String>)> = doc
        .jobs
        .into_iter()
        .filter(|(name, _)| !name.starts_with('_'))
        .map(|(name, job)| (name, job.desc))
        .collect();
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tasks)
}

/// `bacon <job> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("bacon");
    c.arg(task).args(args);
    c
}

#[derive(Deserialize)]
struct BaconDoc {
    #[serde(default)]
    jobs: HashMap<String, JobConfig>,
}

#[derive(Deserialize)]
struct JobConfig {
    #[serde(default, alias = "description")]
    desc: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{detect, extract_tasks};
    use crate::tool::test_support::TempDir;

    #[test]
    fn detect_finds_bacon_toml() {
        let dir = TempDir::new("bacon-detect");
        fs::write(dir.path().join("bacon.toml"), "").expect("bacon.toml should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn detect_returns_false_without_bacon_toml() {
        let dir = TempDir::new("bacon-detect-missing");
        assert!(!detect(dir.path()));
    }

    #[test]
    fn extract_tasks_parses_jobs_table() {
        let dir = TempDir::new("bacon-jobs");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon.toml should parse");

        assert_eq!(tasks, [("check".to_string(), None)]);
    }

    #[test]
    fn extract_tasks_handles_multiple_jobs_sorted() {
        let dir = TempDir::new("bacon-multi");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.test]\ncommand = [\"cargo\", \"test\"]\n\n[jobs.check]\ncommand = [\"cargo\", \"check\"]\n\n[jobs.clippy]\ncommand = [\"cargo\", \"clippy\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon.toml should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(names, ["check", "clippy", "test"]);
    }

    #[test]
    fn extract_tasks_returns_empty_when_no_jobs_table() {
        let dir = TempDir::new("bacon-empty");
        fs::write(dir.path().join("bacon.toml"), "default_job = \"check\"\n")
            .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon.toml should parse");

        assert!(tasks.is_empty());
    }

    #[test]
    fn extract_tasks_surfaces_optional_desc_field() {
        let dir = TempDir::new("bacon-desc");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\ndesc = \"Type-check the workspace\"\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon.toml should parse");

        assert_eq!(
            tasks,
            [(
                "check".to_string(),
                Some("Type-check the workspace".to_string()),
            )]
        );
    }

    #[test]
    fn extract_tasks_accepts_description_alias() {
        let dir = TempDir::new("bacon-description-alias");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\ndescription = \"Long form\"\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon.toml should parse");

        assert_eq!(
            tasks,
            [("check".to_string(), Some("Long form".to_string()))]
        );
    }

    #[test]
    fn extract_tasks_skips_underscore_prefixed_jobs() {
        let dir = TempDir::new("bacon-private");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs._helper]\ncommand = [\"true\"]\n\n[jobs.check]\ncommand = [\"cargo\", \"check\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon.toml should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(names, ["check"]);
    }

    #[test]
    fn extract_tasks_surfaces_parse_error_for_malformed_toml() {
        let dir = TempDir::new("bacon-malformed");
        fs::write(dir.path().join("bacon.toml"), "[jobs.broken")
            .expect("bacon.toml should be written");

        let err = extract_tasks(dir.path()).expect_err("malformed bacon.toml should error");

        assert!(
            err.to_string().contains("failed to parse"),
            "error chain should mention parse failure: {err:#}"
        );
    }
}
