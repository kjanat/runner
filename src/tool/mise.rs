//! mise — polyglot dev tool version manager with a `[tasks]` table for
//! project-local commands (see <https://mise.jdx.dev/tasks/toml-tasks.html>).
//!
//! Detection covers the canonical filenames `mise.toml`, `.mise.toml`, plus
//! the `*.local.toml` companions and the `mise/config.toml` /
//! `.mise/config.toml` / `.config/mise.toml` nested locations.
//!
//! Task extraction prefers `mise tasks --json` when the binary is on
//! `$PATH` — that's the source of truth, merging all config layers
//! (project, env-specific, `.local`, `conf.d`) and surfacing file-based
//! tasks the same way `mise run <name>` will find them. Falls back to
//! direct TOML parsing of the first project-local config when mise
//! isn't installed; the fallback only sees the single file it parses,
//! which is good enough for `runner list` to show a representative view.
//!
//! In both paths we filter to tasks whose `source` lives under the
//! project root so global/system mise tasks don't pollute the project's
//! task list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

/// Project-local config filenames in mise's precedence order (highest first).
///
/// Mise also reads global / system locations (`~/.config/mise/config.toml`,
/// `/etc/mise/config.toml`) and `.config/mise/conf.d/*.toml`; those are
/// out of scope for extraction because they describe the user's environment,
/// not the project's tasks.
pub(crate) const FILENAMES: &[&str] = &[
    "mise.local.toml",
    "mise.toml",
    ".mise.local.toml",
    ".mise.toml",
    "mise/config.toml",
    ".mise/config.toml",
    ".config/mise.toml",
    ".config/mise/config.toml",
];

/// Detected when any [`FILENAMES`] entry resolves to a file under `dir`.
pub(crate) fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

/// Locate the first existing mise config file under `dir`, in precedence
/// order. Returned as an absolute path when the input is absolute.
pub(crate) fn find_file(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, FILENAMES).filter(|path| path.is_file())
}

/// Surface mise tasks defined in this project. Prefers `mise tasks
/// --json` (authoritative across all config layers + file-based tasks),
/// falls back to parsing the first project-local config when mise
/// isn't on `$PATH`.
///
/// Hidden tasks (`hide = true`) and underscore-prefixed names are
/// excluded. Aliases come through as separate `Alias` entries pointing
/// at their target so [`crate::cmd::list`] can group them.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    if let Some(tasks) = extract_tasks_with_cli(dir) {
        return Ok(tasks);
    }
    extract_tasks_from_source(dir)
}

/// Run `mise tasks --json` in `dir` and parse the result. Returns `None`
/// when mise is missing, the invocation fails, or the output doesn't
/// parse — caller falls back to direct TOML reads.
fn extract_tasks_with_cli(dir: &Path) -> Option<Vec<ExtractedTask>> {
    let output = super::program::command("mise")
        .arg("tasks")
        .arg("--json")
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let project_root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    parse_cli_output(&output.stdout, &project_root)
}

/// Parse a `mise tasks --json` payload, filtering to tasks whose
/// `source` lives under `project_root`. Mise's JSON view includes
/// global config and `~/.config/mise/*` tasks; surfacing those in
/// `runner list` would lie about what the project owns.
fn parse_cli_output(stdout: &[u8], project_root: &Path) -> Option<Vec<ExtractedTask>> {
    let entries: Vec<MiseJsonTask> = serde_json::from_slice(stdout).ok()?;
    let mut tasks: Vec<ExtractedTask> = Vec::new();
    for entry in entries {
        if entry.hide || entry.global || entry.name.starts_with('_') {
            continue;
        }
        if !task_belongs_to(&entry.source, project_root) {
            continue;
        }
        let description = entry.description_or_fallback();
        tasks.push(ExtractedTask::Recipe {
            name: entry.name.clone(),
            description,
        });
        push_aliases(&mut tasks, &entry.name, entry.aliases);
    }
    tasks.sort_by(|a, b| a.name().cmp(b.name()));
    Some(tasks)
}

/// Append `Alias` entries for `target` to `tasks`, skipping
/// underscore-prefixed names (mise's own private-task convention) and
/// self-aliases. Shared by [`parse_cli_output`] and
/// [`extract_tasks_from_source`] so the filter rules stay in one place.
fn push_aliases(
    tasks: &mut Vec<ExtractedTask>,
    target: &str,
    aliases: impl IntoIterator<Item = String>,
) {
    for alias in aliases {
        if alias.starts_with('_') || alias == target {
            continue;
        }
        tasks.push(ExtractedTask::Alias {
            name: alias,
            target: target.to_owned(),
        });
    }
}

/// `true` when `source` (mise's `source` path for a task) lives inside
/// `project_root`. Canonicalizes both sides so symlinked checkouts
/// (`/home/x/projects/...` ↔ `/Users/x/projects/...` on macOS) match.
fn task_belongs_to(source: &Path, project_root: &Path) -> bool {
    let canonical = source.canonicalize();
    let candidate = canonical.as_deref().unwrap_or(source);
    candidate.starts_with(project_root)
}

/// Direct-parse fallback for hosts without the `mise` binary. Reads the
/// first existing project-local config (precedence per [`FILENAMES`])
/// and produces the same shape as the CLI path. Only sees one file —
/// mise's cross-file merge isn't replicated.
fn extract_tasks_from_source(dir: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let Some(path) = find_file(dir) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: MiseDoc =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    let mut entries: Vec<ExtractedTask> = Vec::new();
    for (name, task) in doc.tasks {
        if name.starts_with('_') || task.is_hidden() {
            continue;
        }
        let description = task.description();
        let aliases = task.aliases();
        entries.push(ExtractedTask::Recipe {
            name: name.clone(),
            description,
        });
        push_aliases(&mut entries, &name, aliases);
    }
    entries.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(entries)
}

/// One row of `mise tasks --json` output. Mise emits a stable superset
/// of fields; we deserialize only the ones we use and let serde drop
/// the rest.
#[derive(Debug, Deserialize)]
struct MiseJsonTask {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    source: PathBuf,
    #[serde(default)]
    hide: bool,
    /// True for tasks defined in the user's global mise config; we
    /// filter these out so they don't appear as project tasks.
    #[serde(default)]
    global: bool,
    /// `run` is a list of command strings; falls back to the joined
    /// form when `description` is empty.
    #[serde(default)]
    run: Vec<String>,
    /// External script reference; falls back to this when both
    /// `description` and `run` are empty.
    #[serde(default)]
    file: Option<String>,
}

impl MiseJsonTask {
    fn description_or_fallback(&self) -> Option<String> {
        if !self.description.is_empty() {
            return Some(self.description.clone());
        }
        if !self.run.is_empty() {
            return Some(self.run.join(" && "));
        }
        self.file.clone()
    }
}

/// `mise run <task> [-- args...]`
///
/// Mise parses everything after the task name as either positional args
/// for the task's `usage` spec or as additional task names (space-
/// separated) when no `--` is present. Inserting `--` for any caller-
/// supplied args keeps forwarded flags (`--watch`, `--release`) out of
/// mise's own argument parser. Empty arg lists drop the separator so the
/// rendered command line stays clean.
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("mise");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// One task entry surfaced to the rest of the crate. Mirrors
/// [`crate::tool::just::ExtractedTask`] so the detection-layer push helper
/// can stay symmetric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExtractedTask {
    Recipe {
        name: String,
        description: Option<String>,
    },
    Alias {
        name: String,
        target: String,
    },
}

impl ExtractedTask {
    fn name(&self) -> &str {
        match self {
            Self::Recipe { name, .. } | Self::Alias { name, .. } => name,
        }
    }
}

#[derive(Deserialize)]
struct MiseDoc {
    /// Both the `tasks.<name> = "..."` flat form and the
    /// `[tasks.<name>] run = "..."` table form deserialize through
    /// [`TaskEntry`]'s manual `Deserialize` impl.
    #[serde(default)]
    tasks: BTreeMap<String, TaskEntry>,
}

/// Either a bare command string (`build = "cargo build"`) or a full
/// table with `run`/`description`/`alias`/`hide`/`file` fields.
#[derive(Debug)]
struct TaskEntry {
    kind: TaskEntryKind,
}

#[derive(Debug)]
enum TaskEntryKind {
    /// `name = "cargo build"` or `name = ["echo a", "echo b"]`.
    InlineRun(RunField),
    /// `[tasks.name]` table.
    Table(TaskTable),
}

#[derive(Debug, Default, Deserialize)]
struct TaskTable {
    #[serde(default)]
    description: Option<String>,
    /// Mise accepts string-or-array; we only need a representative
    /// value for the description fallback, so untagged enum + `Display`
    /// gives us both shapes for free.
    #[serde(default)]
    run: Option<RunField>,
    /// External script path (local or URL). When set, `run` is usually
    /// absent — the file body provides the commands. Kept here so we
    /// can fall back to it for the description column.
    #[serde(default)]
    file: Option<String>,
    /// `alias = "b"` or `alias = ["b", "build-it"]`.
    #[serde(default)]
    alias: Option<StringOrList>,
    /// `hide = true` excludes the task from listings (mirrors mise's own
    /// `mise tasks ls` behavior).
    #[serde(default)]
    hide: bool,
}

/// Shared shape for both inline (`name = "…"` / `name = ["…", "…"]`) and
/// table-form (`[tasks.name] run = …`) task bodies. Mise accepts a bare
/// string or a string array in either position.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RunField {
    Single(String),
    Multiple(Vec<String>),
}

impl RunField {
    /// Join `Multiple` commands with ` && ` for the description column;
    /// return the single command verbatim. Empty arrays collapse to
    /// `None` so the caller can fall through to other description
    /// sources.
    fn as_description(&self) -> Option<String> {
        match self {
            Self::Single(s) => Some(s.clone()),
            Self::Multiple(v) => (!v.is_empty()).then(|| v.join(" && ")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl TaskEntry {
    const fn is_hidden(&self) -> bool {
        matches!(&self.kind, TaskEntryKind::Table(t) if t.hide)
    }

    /// Best-effort description: explicit `description` first, then the
    /// command body (joined for multi-step `run` arrays), then the
    /// external `file` reference.
    fn description(&self) -> Option<String> {
        match &self.kind {
            TaskEntryKind::InlineRun(run) => run.as_description(),
            TaskEntryKind::Table(t) => t
                .description
                .clone()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| t.run.as_ref().and_then(RunField::as_description))
                .or_else(|| t.file.clone()),
        }
    }

    fn aliases(&self) -> Vec<String> {
        match &self.kind {
            TaskEntryKind::Table(t) => match &t.alias {
                Some(StringOrList::One(s)) => vec![s.clone()],
                Some(StringOrList::Many(v)) => v.clone(),
                None => vec![],
            },
            TaskEntryKind::InlineRun(_) => vec![],
        }
    }
}

impl<'de> Deserialize<'de> for TaskEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Use `toml::Value` as an intermediate so we can pick the
        // representation based on the runtime shape — bare string,
        // array of strings, or full table.
        let value = toml::Value::deserialize(deserializer)?;
        let kind = match value {
            toml::Value::String(s) => TaskEntryKind::InlineRun(RunField::Single(s)),
            toml::Value::Array(arr) => {
                let mut strings = Vec::with_capacity(arr.len());
                for v in arr {
                    match v {
                        toml::Value::String(s) => strings.push(s),
                        other => {
                            return Err(serde::de::Error::custom(format!(
                                "tasks.<name> array must contain strings, got {}",
                                other.type_str()
                            )));
                        }
                    }
                }
                TaskEntryKind::InlineRun(RunField::Multiple(strings))
            }
            toml::Value::Table(_) => {
                let table: TaskTable = value.try_into().map_err(serde::de::Error::custom)?;
                TaskEntryKind::Table(table)
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "tasks.<name> must be a string, array, or table, got {}",
                    other.type_str()
                )));
            }
        };
        Ok(Self { kind })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        ExtractedTask, detect, extract_tasks, extract_tasks_from_source, parse_cli_output, run_cmd,
    };
    use crate::tool::test_support::TempDir;

    #[test]
    fn detect_finds_dot_mise_toml() {
        let dir = TempDir::new("mise-detect-dot");
        fs::write(dir.path().join(".mise.toml"), "").expect(".mise.toml should be written");
        assert!(detect(dir.path()));
    }

    #[test]
    fn detect_finds_mise_toml() {
        let dir = TempDir::new("mise-detect-bare");
        fs::write(dir.path().join("mise.toml"), "").expect("mise.toml should be written");
        assert!(detect(dir.path()));
    }

    #[test]
    fn detect_returns_false_without_mise_config() {
        let dir = TempDir::new("mise-detect-missing");
        assert!(!detect(dir.path()));
    }

    #[test]
    fn run_cmd_omits_separator_when_no_args() {
        let cmd = run_cmd("build", &[]);
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["run", "build"]);
    }

    #[test]
    fn run_cmd_inserts_separator_before_forwarded_args() {
        let cmd = run_cmd("test", &["--watch".into(), "unit".into()]);
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["run", "test", "--", "--watch", "unit"]);
    }

    #[test]
    fn extract_inline_string_task() {
        let dir = TempDir::new("mise-inline-string");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks]\nbuild = \"cargo build\"\ntest = \"cargo test\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");

        assert_eq!(
            tasks,
            [
                ExtractedTask::Recipe {
                    name: "build".to_string(),
                    description: Some("cargo build".to_string()),
                },
                ExtractedTask::Recipe {
                    name: "test".to_string(),
                    description: Some("cargo test".to_string()),
                },
            ],
        );
    }

    #[test]
    fn extract_inline_array_task() {
        let dir = TempDir::new("mise-inline-array");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks]\nci = [\"cargo fmt\", \"cargo clippy\"]\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "ci".to_string(),
                description: Some("cargo fmt && cargo clippy".to_string()),
            }],
        );
    }

    #[test]
    fn extract_table_task_with_description() {
        let dir = TempDir::new("mise-table");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\ndescription = \"Compile the binary\"\nrun = \"cargo build\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "build".to_string(),
                description: Some("Compile the binary".to_string()),
            }],
        );
    }

    #[test]
    fn extract_table_task_falls_back_to_run_when_no_description() {
        let dir = TempDir::new("mise-table-norun");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\nrun = [\"cargo build\", \"cargo test\"]\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "build".to_string(),
                description: Some("cargo build && cargo test".to_string()),
            }],
        );
    }

    #[test]
    fn extract_table_task_treats_blank_description_as_missing() {
        // Mirrors the CLI path's `description_or_fallback`: an empty or
        // whitespace-only `description = ""` shouldn't suppress the
        // run/file fallback. Without the `.filter`, `Some("")` would
        // short-circuit the chain and the task would render with no
        // description at all.
        let dir = TempDir::new("mise-blank-desc");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\ndescription = \"   \"\nrun = \"cargo build\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "build".to_string(),
                description: Some("cargo build".to_string()),
            }],
        );
    }

    #[test]
    fn extract_table_task_with_alias() {
        let dir = TempDir::new("mise-alias");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\nalias = \"b\"\nrun = \"cargo build\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        // Sort is alphabetical by name: "b" < "build".
        assert_eq!(
            tasks,
            [
                ExtractedTask::Alias {
                    name: "b".to_string(),
                    target: "build".to_string(),
                },
                ExtractedTask::Recipe {
                    name: "build".to_string(),
                    description: Some("cargo build".to_string()),
                },
            ],
        );
    }

    #[test]
    fn extract_multiple_aliases_for_one_task() {
        let dir = TempDir::new("mise-alias-many");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\nalias = [\"b\", \"compile\"]\nrun = \"cargo build\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        let names: Vec<&str> = tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect();
        // Sorted alphabetically: b, build, compile.
        assert_eq!(names, ["b", "build", "compile"]);
    }

    #[test]
    fn extract_skips_hidden_tasks() {
        let dir = TempDir::new("mise-hidden");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\nrun = \"cargo build\"\n\n[tasks.helper]\nhide = true\nrun = \"echo nope\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        let names: Vec<&str> = tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_skips_underscore_prefixed_tasks() {
        let dir = TempDir::new("mise-private");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks._helper]\nrun = \"echo nope\"\n\n[tasks.build]\nrun = \"cargo build\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        let names: Vec<&str> = tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_surfaces_file_reference_as_description() {
        let dir = TempDir::new("mise-file-ref");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.lint]\nfile = \"./scripts/lint.sh\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "lint".to_string(),
                description: Some("./scripts/lint.sh".to_string()),
            }],
        );
    }

    #[test]
    fn extract_returns_empty_when_no_tasks_table() {
        let dir = TempDir::new("mise-no-tasks");
        fs::write(dir.path().join(".mise.toml"), "[tools]\nnode = \"22\"\n")
            .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("parse should succeed");
        assert!(tasks.is_empty());
    }

    #[test]
    fn extract_surfaces_parse_error_for_malformed_toml() {
        let dir = TempDir::new("mise-malformed");
        fs::write(dir.path().join(".mise.toml"), "[tasks.build")
            .expect(".mise.toml should be written");

        let err = extract_tasks(dir.path()).expect_err("malformed .mise.toml should error");
        assert!(
            err.to_string().contains("failed to parse"),
            "error chain should mention parse failure: {err:#}",
        );
    }

    #[test]
    fn cli_output_extracts_tasks_under_project_root() {
        // Captured shape from `mise tasks --json` against the
        // dprint-plugin-shfmt repo (see issue #23 follow-up).
        let dir = TempDir::new("mise-cli-payload");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let source_path = project.join(".config/mise.toml");
        let payload = serde_json::json!([
            {
                "name": "build-wasm",
                "aliases": ["bw"],
                "description": "Build wasm plugin and schema",
                "source": source_path.to_string_lossy(),
                "hide": false,
                "global": false,
                "run": ["go run ./dprint/cmd/build"],
                "file": null,
            },
            {
                "name": "test",
                "aliases": [],
                "description": "Run Go tests",
                "source": source_path.to_string_lossy(),
                "hide": false,
                "global": false,
                "run": ["go test ./..."],
                "file": null,
            },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");

        // Alphabetical sort: "build-wasm" < "bw" because '-' (0x2D)
        // sorts before 'w' (0x77).
        assert_eq!(
            tasks,
            [
                ExtractedTask::Recipe {
                    name: "build-wasm".to_string(),
                    description: Some("Build wasm plugin and schema".to_string()),
                },
                ExtractedTask::Alias {
                    name: "bw".to_string(),
                    target: "build-wasm".to_string(),
                },
                ExtractedTask::Recipe {
                    name: "test".to_string(),
                    description: Some("Run Go tests".to_string()),
                },
            ],
        );
    }

    #[test]
    fn cli_output_filters_tasks_outside_project_root() {
        // Global tasks (from `~/.config/mise/config.toml`) show up in
        // `mise tasks --json` too — they must not pollute the
        // project's task list.
        let dir = TempDir::new("mise-cli-global-filter");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let payload = serde_json::json!([
            {
                "name": "project-task",
                "aliases": [],
                "description": "Local",
                "source": project.join("mise.toml").to_string_lossy(),
                "hide": false, "global": false, "run": ["echo local"], "file": null,
            },
            {
                "name": "global-task",
                "aliases": [],
                "description": "Global",
                "source": "/home/whoever/.config/mise/config.toml",
                "hide": false, "global": true, "run": ["echo global"], "file": null,
            },
            {
                "name": "sibling-task",
                "aliases": [],
                "description": "Sibling repo",
                "source": "/tmp/other-project/mise.toml",
                "hide": false, "global": false, "run": ["echo other"], "file": null,
            },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        let names: Vec<&str> = tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect();
        assert_eq!(names, ["project-task"]);
    }

    #[test]
    fn cli_output_falls_back_to_run_when_description_missing() {
        let dir = TempDir::new("mise-cli-desc-fallback");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let payload = serde_json::json!([
            {
                "name": "ci",
                "aliases": [],
                "description": "",
                "source": project.join("mise.toml").to_string_lossy(),
                "hide": false, "global": false,
                "run": ["cargo fmt", "cargo clippy"],
                "file": null,
            },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "ci".to_string(),
                description: Some("cargo fmt && cargo clippy".to_string()),
            }],
        );
    }

    #[test]
    fn cli_output_falls_back_to_file_when_run_and_description_missing() {
        let dir = TempDir::new("mise-cli-file-fallback");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let payload = serde_json::json!([
            {
                "name": "lint",
                "aliases": [],
                "description": "",
                "source": project.join("mise.toml").to_string_lossy(),
                "hide": false, "global": false,
                "run": [],
                "file": "./scripts/lint.sh",
            },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "lint".to_string(),
                description: Some("./scripts/lint.sh".to_string()),
            }],
        );
    }

    #[test]
    fn cli_output_skips_hidden_and_underscore_prefixed() {
        let dir = TempDir::new("mise-cli-hidden");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let src = project.join("mise.toml").to_string_lossy().to_string();
        let payload = serde_json::json!([
            { "name": "build", "aliases": [], "description": "", "source": src, "hide": false, "global": false, "run": ["echo build"], "file": null },
            { "name": "helper", "aliases": [], "description": "", "source": src, "hide": true,  "global": false, "run": ["echo nope"], "file": null },
            { "name": "_private", "aliases": [], "description": "", "source": src, "hide": false, "global": false, "run": ["echo nope"], "file": null },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        let names: Vec<&str> = tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn cli_output_returns_none_for_malformed_json() {
        let dir = TempDir::new("mise-cli-bad-json");
        let project = dir.path().to_path_buf();
        assert!(parse_cli_output(b"not json", &project).is_none());
    }

    #[test]
    fn extract_uses_mise_cli_when_available() {
        // When mise is on PATH, the fast path should pick up tasks
        // that the direct TOML parser can't see (cross-file merges,
        // file-based tasks). Skip silently when mise isn't installed.
        if std::process::Command::new("mise")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: mise unavailable");
            return;
        }

        let dir = TempDir::new("mise-cli-fast-path");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.build]\ndescription = \"build it\"\nrun = \"echo build\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path()).expect("mise CLI should succeed");
        let has_build = tasks.iter().any(|t| {
            matches!(t,
            ExtractedTask::Recipe { name, .. } if name == "build")
        });
        assert!(has_build, "fast path should surface `build`; got {tasks:?}");
    }

    #[test]
    fn extract_prefers_higher_precedence_file() {
        // `mise.toml` outranks `.mise.toml`; the latter should be
        // ignored when both exist. (Mise itself merges, but we only
        // need to surface a representative task list for `runner list`.)
        //
        // Exercise the file-precedence path directly: `extract_tasks`
        // routes through `mise tasks --json` first when the binary is
        // on `$PATH`, which would return a merged view across both
        // files and defeat the assertion. The CLI fast path is
        // covered separately by `extract_uses_mise_cli_when_available`.
        let dir = TempDir::new("mise-precedence");
        fs::write(
            dir.path().join("mise.toml"),
            "[tasks]\nfrom-mise-toml = \"echo a\"\n",
        )
        .expect("mise.toml should be written");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks]\nfrom-dot-mise-toml = \"echo b\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("parse should succeed");
        let names: Vec<&str> = tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect();
        assert_eq!(names, ["from-mise-toml"]);
    }
}
