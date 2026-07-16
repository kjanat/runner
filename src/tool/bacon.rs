//! bacon, a Rust background checker driven by `bacon.toml`.
//!
//! Surfaces every runnable job. When the `bacon` CLI is available the fast
//! path runs `bacon --list-jobs` so we see the same merged view bacon itself
//! presents (built-in jobs + user overrides + project-local additions). When
//! bacon isn't installed we fall back to parsing `bacon.toml` directly,
//! which only sees the project-declared jobs; built-ins live in the bacon
//! binary and there's no way to enumerate them without invoking it.

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
/// Prefers `bacon --list-jobs` (the source of truth: it merges bacon's
/// baked-in jobs with whatever `bacon.toml` declares), falling back to
/// parsing `bacon.toml` directly when the binary is missing or its
/// output won't parse.
///
/// Jobs whose names start with `_` are hidden (just-style convention).
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    if let Some(tasks) = extract_tasks_with_bacon(dir) {
        return Ok(tasks);
    }
    extract_tasks_from_source(dir)
}

fn extract_tasks_with_bacon(dir: &Path) -> Option<Vec<(String, Option<String>)>> {
    // Bacon sizes the output table to the terminal width, truncating long
    // command strings with no marker when they don't fit (a job like
    // `cargo clippy --all-targets --all-features -- -D warnings` becomes
    // `…--all-features -- -D` on an 80-col terminal). Override `COLUMNS`
    // to a value comfortably wider than any realistic command so the
    // description column reaches us intact; bacon honours the env var.
    let output = super::program::command("bacon")
        .arg("--list-jobs")
        .current_dir(dir)
        .env("COLUMNS", "10000")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_list_jobs_table(&output.stdout)
}

/// Parse the ASCII-bordered `job │ command` table from `bacon
/// --list-jobs`. ANSI styling is stripped (CSI `m` form) before
/// splitting on the unicode `│` separator; the `job` header row is
/// skipped by literal cell match.
fn parse_list_jobs_table(stdout: &[u8]) -> Option<Vec<(String, Option<String>)>> {
    let stripped = strip_csi_m(&String::from_utf8_lossy(stdout));
    let mut tasks: Vec<(String, Option<String>)> = Vec::new();
    let mut in_body = false;
    for line in stripped.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('├') {
            in_body = true;
            continue;
        }
        if !in_body {
            continue;
        }
        if trimmed.starts_with('└') {
            break;
        }
        if !trimmed.starts_with('│') {
            continue;
        }

        let cells: Vec<&str> = trimmed.split('│').collect();
        if cells.len() < 4 {
            continue;
        }
        let name = cells[1].trim();
        let command = cells[2].trim();
        if name.is_empty() || name == "job" || name.starts_with('_') {
            continue;
        }
        let desc = (!command.is_empty()).then(|| command.to_string());
        tasks.push((name.to_string(), desc));
    }
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    (!tasks.is_empty()).then_some(tasks)
}

/// Strip CSI `m` (SGR color/bold/style) escape sequences. Hand-rolled:
/// the form is narrow (`\x1b[…m`) and doesn't justify a `regex` dep.
fn strip_csi_m(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for inner in chars.by_ref() {
                // CSI parameter bytes are `0x30–0x3F`, intermediate `0x20–0x2F`,
                // final `0x40–0x7E`. We bail on the final `m` specifically since
                // that's the only form bacon emits; anything else stays literal,
                // which is fine because bacon doesn't use other CSI types here.
                if inner == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn extract_tasks_from_source(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
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
        .map(|(name, job)| {
            // Surface the command array (joined with spaces) when no
            // `desc` is set, so `runner list` shows the same description
            // shape as the CLI fast path. Stock bacon doesn't define a
            // description field on jobs; keep `desc`/`description` as
            // forward-compatible overrides.
            let desc = job
                .desc
                .or_else(|| (!job.command.is_empty()).then(|| job.command.join(" ")));
            (name, desc)
        })
        .collect();
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tasks)
}

/// `bacon <job> [-- args...]`
///
/// Bacon's CLI is `bacon [options] [ARGS] [-- ADDITIONAL_JOB_ARGS]`: any
/// extra args without the `--` separator get parsed as bacon's own options
/// (or as a project path), so a value like `--ignored` would either be
/// rejected as an unknown flag or, worse, interpreted as a bacon option. We
/// always insert `--` when args are present so flags and positionals reach
/// the underlying job verbatim.
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("bacon");
    c.arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
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
    #[serde(default)]
    command: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        detect, extract_tasks, extract_tasks_from_source, parse_list_jobs_table, run_cmd,
        strip_csi_m,
    };
    use crate::tool::test_support::TempDir;

    #[test]
    fn run_cmd_omits_separator_when_no_args() {
        let cmd = run_cmd("check", &[]);
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();

        assert_eq!(argv, ["check"]);
    }

    #[test]
    fn run_cmd_inserts_separator_before_forwarded_args() {
        // Bacon parses anything after the job name as its own options unless
        // separated by `--`. Without the separator, `--ignored` would error
        // out as an unknown bacon flag.
        let cmd = run_cmd("test", &["--ignored".into(), "my_test".into()]);
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();

        assert_eq!(argv, ["test", "--", "--ignored", "my_test"]);
    }

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
    fn parse_list_jobs_extracts_name_and_command() {
        // Captured from bacon 3.22.0 `bacon --list-jobs` (with ANSI styling
        // present so the stripper is exercised on real input).
        let raw = "\u{1b}[38;5;239m┌─┬─┐\u{1b}[39m\n\
                   \u{1b}[38;5;239m│\u{1b}[39m   \u{1b}[1mjob\u{1b}[0m    \u{1b}[38;5;239m│\u{1b}[39m\u{1b}[1mcommand\u{1b}[0m         │\n\
                   \u{1b}[38;5;239m├─┼─┤\u{1b}[39m\n\
                   \u{1b}[38;5;239m│\u{1b}[39m  check   \u{1b}[38;5;239m│\u{1b}[39mcargo check     │\n\
                   \u{1b}[38;5;239m│\u{1b}[39m clippy   \u{1b}[38;5;239m│\u{1b}[39mcargo clippy    │\n\
                   \u{1b}[38;5;239m└─┴─┘\u{1b}[39m\n\
                   default job: check\n";

        let tasks = parse_list_jobs_table(raw.as_bytes()).expect("table should parse");

        assert_eq!(
            tasks,
            [
                ("check".to_string(), Some("cargo check".to_string())),
                ("clippy".to_string(), Some("cargo clippy".to_string())),
            ]
        );
    }

    #[test]
    fn parse_list_jobs_skips_underscore_prefixed_rows() {
        let raw = "┌─┬─┐\n│  job   │command         │\n├─┼─┤\n│_helper │true            │\n│ \
                   check  │cargo check     │\n└─┴─┘\n";

        let tasks = parse_list_jobs_table(raw.as_bytes()).expect("table should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(names, ["check"]);
    }

    #[test]
    fn parse_list_jobs_returns_none_for_empty_table() {
        // Header-only table (no body rows) shouldn't masquerade as success.
        let raw = "┌─┬─┐\n│ job │command│\n├─┼─┤\n└─┴─┘\n";

        assert!(parse_list_jobs_table(raw.as_bytes()).is_none());
    }

    #[test]
    fn strip_csi_m_removes_sgr_sequences() {
        let raw = "\u{1b}[38;5;239m│\u{1b}[39m\u{1b}[1mhello\u{1b}[0m";
        assert_eq!(strip_csi_m(raw), "│hello");
    }

    #[test]
    fn extract_tasks_from_source_parses_jobs_table() {
        let dir = TempDir::new("bacon-jobs");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("bacon.toml should parse");

        assert_eq!(
            tasks,
            [("check".to_string(), Some("cargo check".to_string()))]
        );
    }

    #[test]
    fn extract_tasks_from_source_handles_multiple_jobs_sorted() {
        let dir = TempDir::new("bacon-multi");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.test]\ncommand = [\"cargo\", \"test\"]\n\n[jobs.check]\ncommand = [\"cargo\", \
             \"check\"]\n\n[jobs.clippy]\ncommand = [\"cargo\", \"clippy\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("bacon.toml should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(names, ["check", "clippy", "test"]);
    }

    #[test]
    fn extract_tasks_from_source_returns_empty_when_no_jobs_table() {
        let dir = TempDir::new("bacon-empty");
        fs::write(dir.path().join("bacon.toml"), "default_job = \"check\"\n")
            .expect("bacon.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("bacon.toml should parse");

        assert!(tasks.is_empty());
    }

    #[test]
    fn extract_tasks_from_source_surfaces_optional_desc_field() {
        let dir = TempDir::new("bacon-desc");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\ndesc = \"Type-check the workspace\"\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("bacon.toml should parse");

        assert_eq!(
            tasks,
            [(
                "check".to_string(),
                Some("Type-check the workspace".to_string()),
            )]
        );
    }

    #[test]
    fn extract_tasks_from_source_accepts_description_alias() {
        let dir = TempDir::new("bacon-description-alias");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs.check]\ncommand = [\"cargo\", \"check\"]\ndescription = \"Long form\"\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("bacon.toml should parse");

        assert_eq!(
            tasks,
            [("check".to_string(), Some("Long form".to_string()))]
        );
    }

    #[test]
    fn extract_tasks_from_source_skips_underscore_prefixed_jobs() {
        let dir = TempDir::new("bacon-private");
        fs::write(
            dir.path().join("bacon.toml"),
            "[jobs._helper]\ncommand = [\"true\"]\n\n[jobs.check]\ncommand = [\"cargo\", \
             \"check\"]\n",
        )
        .expect("bacon.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("bacon.toml should parse");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(names, ["check"]);
    }

    #[test]
    fn extract_tasks_from_source_surfaces_parse_error_for_malformed_toml() {
        let dir = TempDir::new("bacon-malformed");
        fs::write(dir.path().join("bacon.toml"), "[jobs.broken")
            .expect("bacon.toml should be written");

        let err =
            extract_tasks_from_source(dir.path()).expect_err("malformed bacon.toml should error");

        assert!(
            err.to_string().contains("failed to parse"),
            "error chain should mention parse failure: {err:#}"
        );
    }

    #[test]
    fn extract_tasks_uses_bacon_cli_when_available() {
        // When bacon is installed, the fast path should pull in built-in
        // jobs (e.g. `check`, `test`, `clippy`) on top of whatever the
        // local `bacon.toml` declares; that's the whole point of
        // shelling out instead of parsing the TOML alone. Skip silently
        // when bacon isn't on PATH so this stays portable.
        if std::process::Command::new("bacon")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: bacon unavailable");
            return;
        }

        let dir = TempDir::new("bacon-cli-fast-path");
        fs::write(dir.path().join("bacon.toml"), "default_job = \"check\"\n")
            .expect("bacon.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("bacon CLI should succeed");
        let names: Vec<&str> = tasks.iter().map(|(n, _)| n.as_str()).collect();

        assert!(
            names.contains(&"check"),
            "fast path should surface the built-in `check` job; got {names:?}"
        );
    }
}
