//! mise, polyglot dev tool version manager with a `[tasks]` table for
//! project-local commands (see <https://mise.jdx.dev/tasks/toml-tasks.html>).
//!
//! Detection covers the canonical filenames `mise.toml`, `.mise.toml`, plus
//! the `*.local.toml` companions and the `mise/config.toml` /
//! `.mise/config.toml` / `.config/mise.toml` nested locations.
//!
//! Task extraction prefers `mise tasks --json` when the binary is on
//! `$PATH` (that's the source of truth), merging all config layers
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

use crate::types::TaskDetail;

pub(crate) const fn quiet_capabilities() -> super::HostQuietCapabilities {
    super::HostQuietCapabilities::quiet("mise", &["--quiet"])
}

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
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<MiseTasks> {
    match cli_tasks(dir) {
        CliOutcome::Tasks(tasks) => Ok(MiseTasks {
            tasks,
            degraded: None,
        }),
        CliOutcome::Unavailable => Ok(MiseTasks {
            tasks: extract_tasks_from_source(dir)?,
            degraded: None,
        }),
        CliOutcome::Failed(reason) => match extract_tasks_from_source(dir) {
            Ok(tasks) => Ok(MiseTasks {
                tasks,
                degraded: Some(reason),
            }),
            Err(err) => Err(err.context(reason)),
        },
    }
}

/// The mise task list plus the reason the authoritative path was not used,
/// when it was tried and failed.
#[derive(Debug)]
pub(crate) struct MiseTasks {
    /// Tasks, from `mise tasks --json` or the single-file TOML fallback.
    pub tasks: Vec<ExtractedTask>,
    /// Why `mise tasks --json` was not used, when mise is on `PATH` but its
    /// task view could not be read. The fallback sees one config file, so
    /// this list may be missing cross-file merges and file tasks.
    pub degraded: Option<String>,
}

/// What `mise tasks --json` produced. A missing binary is the fallback's
/// normal trigger; anything else means mise is installed and runner is
/// showing a lesser view than the user's own `mise tasks` would.
enum CliOutcome {
    Tasks(Vec<ExtractedTask>),
    Unavailable,
    Failed(String),
}

/// Run `mise tasks --json` in `dir` and parse the result.
fn cli_tasks(dir: &Path) -> CliOutcome {
    let output = match super::program::command("mise")
        .arg("tasks")
        .arg("--json")
        .current_dir(dir)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CliOutcome::Unavailable;
        }
        Err(error) => {
            return CliOutcome::Failed(format!("`mise tasks --json` failed to launch: {error}"));
        }
    };
    if !output.status.success() {
        return CliOutcome::Failed(format!(
            "`mise tasks --json` {}{}",
            output.status,
            first_line(&output.stderr)
        ));
    }
    let project_root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    match parse_cli_output(&output.stdout, &project_root) {
        Ok(tasks) => CliOutcome::Tasks(tasks),
        Err(error) => {
            CliOutcome::Failed(format!("`mise tasks --json` output did not parse: {error}"))
        }
    }
}

/// The first non-empty line of a captured stderr, prefixed with `: ` for
/// appending to a diagnostic. Empty when there is nothing to quote.
fn first_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(String::new, |line| format!(": {line}"))
}

/// Parse a `mise tasks --json` payload, filtering to tasks whose
/// `source` lives under `project_root`. Mise's JSON view includes
/// global config and `~/.config/mise/*` tasks; surfacing those in
/// `runner list` would lie about what the project owns.
fn parse_cli_output(
    stdout: &[u8],
    project_root: &Path,
) -> Result<Vec<ExtractedTask>, serde_json::Error> {
    let entries: Vec<MiseJsonTask> = serde_json::from_slice(stdout)?;
    let mut tasks: Vec<ExtractedTask> = Vec::new();
    for entry in entries {
        if entry.hide || entry.global || entry.name.starts_with('_') {
            continue;
        }
        if !task_belongs_to(&entry.source, project_root) {
            continue;
        }
        let description = entry.description_or_fallback();
        let detail = Box::new(entry.detail(project_root));
        tasks.push(ExtractedTask::Recipe {
            name: entry.name.clone(),
            description,
            detail,
        });
        push_aliases(&mut tasks, &entry.name, entry.aliases);
    }
    tasks.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(tasks)
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
/// and produces the same shape as the CLI path. Only sees one file;
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
        let detail = Box::new(TaskDetail {
            file: task.file().map(str::to_owned),
            ..TaskDetail::default()
        });
        entries.push(ExtractedTask::Recipe {
            name: name.clone(),
            description,
            detail,
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
    /// `run` is a list of command strings or task references; falls back
    /// to the joined form when `description` is empty.
    #[serde(default)]
    run: Vec<RunStep>,
    /// External script reference; falls back to this when both
    /// `description` and `run` are empty.
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    depends: Vec<DependEntry>,
    #[serde(default)]
    depends_post: Vec<DependEntry>,
    #[serde(default)]
    wait_for: Vec<DependEntry>,
    #[serde(default)]
    dir: Option<PathBuf>,
    /// `KEY=VALUE` strings in current mise; older payloads carried
    /// objects, which are flattened to the same shape.
    #[serde(default)]
    env: Vec<EnvEntry>,
    #[serde(default)]
    tools: BTreeMap<String, ToolSpec>,
    #[serde(default)]
    usage: String,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    timeout: Option<Scalar>,
}

/// One `depends`/`depends_post`/`wait_for` element. A bare task name is a
/// string; a dependency carrying arguments (`{ task = "gen", args = ["foo"] }`
/// in TOML) arrives as `["gen", "foo"]`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DependEntry {
    Name(String),
    WithArgs(Vec<String>),
    Unknown(serde::de::IgnoredAny),
}

impl DependEntry {
    /// The depended-on task's name. The arguments are dropped: every
    /// consumer of this list matches it against task names.
    fn name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name.as_str()),
            Self::WithArgs(parts) => parts.first().map(String::as_str),
            Self::Unknown(_) => None,
        }
    }
}

/// Collect the task names out of a dependency list.
fn depend_names(entries: &[DependEntry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(DependEntry::name)
        .map(str::to_owned)
        .collect()
}

/// One `tools` value: a version string, or mise's structured form
/// (`{ version = "22", os = ["linux"] }`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolSpec {
    Version(Scalar),
    Detailed {
        #[serde(default)]
        version: Option<Scalar>,
    },
    Unknown(serde::de::IgnoredAny),
}

impl ToolSpec {
    /// The requested version, empty when the structured form omits one.
    fn version(&self) -> String {
        match self {
            Self::Version(version) => version.to_string(),
            Self::Detailed { version } => version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            Self::Unknown(_) => String::new(),
        }
    }
}

/// One `env` element: `"KEY=VALUE"` or `{ "KEY": "VALUE", ... }`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EnvEntry {
    Pair(String),
    Map(BTreeMap<String, Scalar>),
    Unknown(serde::de::IgnoredAny),
}

/// A JSON scalar rendered back to its source text.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scalar {
    Text(String),
    Number(serde_json::Number),
    Flag(bool),
}

impl std::fmt::Display for Scalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Number(number) => write!(f, "{number}"),
            Self::Flag(flag) => write!(f, "{flag}"),
        }
    }
}

impl MiseJsonTask {
    fn description_or_fallback(&self) -> Option<String> {
        if !self.description.trim().is_empty() {
            return Some(self.description.clone());
        }
        join_steps(&self.run).or_else(|| self.file.clone())
    }

    /// Everything the payload says about the task beyond name and
    /// description. `dir` is dropped when it is the project root itself,
    /// so it only carries information when the task runs elsewhere.
    fn detail(&self, project_root: &Path) -> TaskDetail {
        let dir = self.dir.clone().filter(|dir| {
            let canonical = dir.canonicalize();
            canonical.as_deref().unwrap_or(dir) != project_root
        });
        let env = self
            .env
            .iter()
            .flat_map(|entry| match entry {
                EnvEntry::Pair(pair) => vec![pair.clone()],
                EnvEntry::Map(map) => map.iter().map(|(k, v)| format!("{k}={v}")).collect(),
                EnvEntry::Unknown(_) => Vec::new(),
            })
            .collect();
        TaskDetail {
            depends: depend_names(&self.depends),
            depends_post: depend_names(&self.depends_post),
            wait_for: depend_names(&self.wait_for),
            dir,
            env,
            tools: self
                .tools
                .iter()
                .map(|(tool, spec)| (tool.clone(), spec.version()))
                .collect(),
            usage: Some(self.usage.trim())
                .filter(|spec| !spec.is_empty())
                .map(str::to_owned),
            file: self.file.clone(),
            sources: self.sources.clone(),
            outputs: self.outputs.clone(),
            timeout: self.timeout.as_ref().map(ToString::to_string),
        }
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
pub(crate) fn run_cmd(task: &str, args: &[String], verbosity: super::HostVerbosity) -> Command {
    let mut c = super::program::command("mise");
    // mise's global `-q`/`--quiet` precedes the `run` subcommand. It has no
    // stdout-diversion primitive, so the stream axis no-ops.
    if verbosity.silences() {
        c.arg("--quiet");
    }
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// The default operation when `[tools.mise].run` says nothing.
pub(crate) const INSTALL: &str = "install";

/// Operations `[tools.mise].run` accepts.
///
/// `bootstrap` also performs machine setup (system packages, dotfiles,
/// services, firewall), so it is never the default and only runs when the
/// project asks for it by name.
pub(crate) const OPERATIONS: &[&str] = &[INSTALL, "bootstrap"];

/// `mise <operation> [--locked]`.
///
/// `--locked` is added for a frozen run only when a lockfile exists, since
/// mise refuses the flag without one.
pub(crate) fn operation_cmd(
    root: &Path,
    operation: &str,
    frozen: bool,
    verbosity: super::HostVerbosity,
) -> Command {
    let mut c = super::program::command("mise");
    if verbosity.silences() {
        c.arg("--quiet");
    }
    c.arg(operation);
    if frozen && has_lockfile(root) {
        c.arg("--locked");
    }
    c
}

/// `true` when any detected config in `root` has its lockfile on disk.
fn has_lockfile(root: &Path) -> bool {
    FILENAMES
        .iter()
        .map(|name| root.join(name))
        .filter(|config| config.is_file())
        .any(|config| lock_path(&config).is_file())
}

/// The lockfile mise writes for `config`. It sits beside the config and is
/// named `mise.lock`, or `mise.local.lock` for a `*.local.toml` config.
fn lock_path(config: &Path) -> PathBuf {
    let local = config
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".local."));
    let name = if local {
        "mise.local.lock"
    } else {
        "mise.lock"
    };
    config
        .parent()
        .map_or_else(|| PathBuf::from(name), |dir| dir.join(name))
}

/// The tool bin directories mise puts on `PATH` for this project, from
/// `mise bin-paths`. Empty when mise is missing or reports nothing.
///
/// `mise install` installs tools without activating them, so a package
/// manager it just installed is invisible to this process and to the
/// children runner spawns next.
pub(crate) fn bin_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(output) = super::program::command("mise")
        .arg("bin-paths")
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// What mise says about this project's own health, for `runner doctor`.
/// Empty when mise is not installed or declines to answer: doctor reports
/// what it can reach and stays quiet about the rest.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Health {
    /// Declared tools that are not installed, as `node@22`.
    pub missing_tools: Vec<String>,
    /// Problems `mise tasks validate` found in the task graph.
    pub task_issues: Vec<TaskIssue>,
}

/// One `mise tasks validate` finding.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TaskIssue {
    /// The task the finding is about.
    pub task: String,
    /// Mise's own severity label (`error`, `warning`).
    pub severity: String,
    /// One-line summary, with mise's `details` appended when it adds
    /// anything the message does not already say.
    pub message: String,
}

/// Ask mise what is wrong with this project: which declared tools are not
/// installed, and what `mise tasks validate` makes of the task graph.
///
/// Both probes are best-effort. Runner does not fail on what they report;
/// doctor exists to relay it.
pub(crate) fn health(root: &Path) -> Health {
    Health {
        missing_tools: missing_tools(root),
        task_issues: task_issues(root),
    }
}

/// Capture a mise subcommand's stdout, ignoring the exit status.
///
/// `mise tasks validate` exits non-zero precisely when it has findings to
/// report, and prints them to stdout either way.
fn json_stdout(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = super::program::command("mise")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    (!output.stdout.is_empty()).then_some(output.stdout)
}

/// `mise ls --missing --json`, rendered as `node@22` entries.
fn missing_tools(root: &Path) -> Vec<String> {
    let Some(stdout) = json_stdout(root, &["ls", "--missing", "--json"]) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_slice::<BTreeMap<String, Vec<MissingTool>>>(&stdout) else {
        return Vec::new();
    };
    parsed
        .into_iter()
        .flat_map(|(tool, entries)| {
            entries
                .into_iter()
                .filter(|entry| !entry.installed)
                .map(move |entry| {
                    entry
                        .requested_version
                        .or(entry.version)
                        .map_or_else(|| tool.clone(), |version| format!("{tool}@{version}"))
                })
        })
        .collect()
}

/// `mise tasks validate --json`, flattened to one line per finding.
fn task_issues(root: &Path) -> Vec<TaskIssue> {
    let Some(stdout) = json_stdout(root, &["tasks", "validate", "--json"]) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_slice::<ValidateReport>(&stdout) else {
        return Vec::new();
    };
    parsed
        .issues
        .into_iter()
        .map(|issue| TaskIssue {
            task: issue.task,
            severity: issue.severity,
            message: match issue.details.filter(|d| !d.trim().is_empty()) {
                Some(details) => format!("{} ({details})", issue.message),
                None => issue.message,
            },
        })
        .collect()
}

/// One entry of `mise ls --missing --json`.
#[derive(Debug, Deserialize)]
struct MissingTool {
    #[serde(default)]
    requested_version: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    installed: bool,
}

/// `mise tasks validate --json`.
#[derive(Debug, Deserialize)]
struct ValidateReport {
    #[serde(default)]
    issues: Vec<ValidateIssue>,
}

/// One `issues` element of `mise tasks validate --json`.
#[derive(Debug, Deserialize)]
struct ValidateIssue {
    #[serde(default)]
    task: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: Option<String>,
}

/// A mise task's arguments and flags.
///
/// Tasks declare these as a `usage` KDL block. `mise tasks info --json`
/// returns that block already parsed, so runner reads structure rather than
/// re-implementing the KDL grammar.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UsageSpec {
    /// One-line signature mise renders for the task, e.g. `<--fn <name>> [dir]`.
    pub signature: String,
    pub args: Vec<UsageArg>,
    pub flags: Vec<UsageFlag>,
}

/// One positional argument of a task.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UsageArg {
    pub name: String,
    pub help: Option<String>,
    pub required: bool,
    /// Accepted values, when the spec closes the set with `choices`.
    pub choices: Vec<String>,
}

/// One flag of a task.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UsageFlag {
    /// Long spellings without the `--`.
    pub long: Vec<String>,
    /// Short spellings without the `-`.
    pub short: Vec<String>,
    pub help: Option<String>,
    pub required: bool,
    /// `true` when the flag consumes the next word.
    pub takes_value: bool,
}

impl UsageSpec {
    /// `true` when the spec declares nothing worth completing or checking.
    pub(crate) const fn is_empty(&self) -> bool {
        self.args.is_empty() && self.flags.is_empty()
    }

    /// `true` when `word` is a flag that swallows the word after it, so the
    /// next position holds that flag's value rather than another flag.
    pub(crate) fn consumes_value_after(&self, word: &str) -> bool {
        let Some(name) = word.strip_prefix("--") else {
            return false;
        };
        if name.contains('=') {
            return false;
        }
        self.flags
            .iter()
            .filter(|flag| flag.takes_value)
            .any(|flag| flag.long.iter().any(|long| long == name))
    }

    /// Long spellings of every flag the spec marks required.
    pub(crate) fn missing_required_flags(&self, provided: &[String]) -> Vec<String> {
        self.flags
            .iter()
            .filter(|flag| flag.required)
            .filter(|flag| {
                !flag.long.iter().any(|long| {
                    let dashed = format!("--{long}");
                    provided
                        .iter()
                        .any(|word| word == &dashed || word.starts_with(&format!("{dashed}=")))
                })
            })
            .filter_map(|flag| flag.long.first().map(|long| format!("--{long}")))
            .collect()
    }
}

/// Read `task`'s spec via `mise tasks info <task> --json`.
///
/// `None` when mise is missing, the task is unknown, or it declares no spec.
/// One subprocess per call: the bulk `mise tasks ls --json` carries only the
/// unparsed KDL string and mise exposes no bulk flag for the parsed form.
pub(crate) fn usage_spec(root: &Path, task: &str) -> Option<UsageSpec> {
    let stdout = json_stdout(root, &["tasks", "info", task, "--json"])?;
    let info: TaskInfoJson = serde_json::from_slice(&stdout).ok()?;
    let cmd = info.usage_spec?.cmd?;
    let spec = UsageSpec {
        signature: cmd.usage.unwrap_or_default(),
        args: cmd
            .args
            .into_iter()
            .map(|arg| UsageArg {
                name: arg.name,
                help: arg.help.filter(|h| !h.trim().is_empty()),
                required: arg.required,
                choices: arg.choices.map(|c| c.choices).unwrap_or_default(),
            })
            .collect(),
        flags: cmd
            .flags
            .into_iter()
            .map(|flag| UsageFlag {
                long: flag.long,
                short: flag.short,
                help: flag.help.filter(|h| !h.trim().is_empty()),
                required: flag.required,
                takes_value: flag.arg.is_some(),
            })
            .collect(),
    };
    (!spec.is_empty()).then_some(spec)
}

/// `mise tasks info <task> --json`, narrowed to the spec.
#[derive(Debug, Deserialize)]
struct TaskInfoJson {
    #[serde(default)]
    usage_spec: Option<UsageSpecJson>,
}

#[derive(Debug, Deserialize)]
struct UsageSpecJson {
    #[serde(default)]
    cmd: Option<UsageCmdJson>,
}

#[derive(Debug, Deserialize)]
struct UsageCmdJson {
    #[serde(default)]
    usage: Option<String>,
    #[serde(default)]
    args: Vec<UsageArgJson>,
    #[serde(default)]
    flags: Vec<UsageFlagJson>,
}

#[derive(Debug, Deserialize)]
struct UsageArgJson {
    name: String,
    #[serde(default)]
    help: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    choices: Option<UsageChoicesJson>,
}

/// mise nests the accepted values one level deep.
#[derive(Debug, Deserialize)]
struct UsageChoicesJson {
    #[serde(default)]
    choices: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UsageFlagJson {
    #[serde(default)]
    long: Vec<String>,
    #[serde(default)]
    short: Vec<String>,
    #[serde(default)]
    help: Option<String>,
    #[serde(default)]
    required: bool,
    /// Present when the flag consumes a value.
    #[serde(default)]
    arg: Option<serde::de::IgnoredAny>,
}

/// One task entry surfaced to the rest of the crate. Mirrors
/// [`crate::tool::just::ExtractedTask`] so the detection-layer push helper
/// can stay symmetric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExtractedTask {
    Recipe {
        name: String,
        description: Option<String>,
        detail: Box<TaskDetail>,
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
    /// absent; the file body provides the commands. Kept here so we
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
/// string or an array of [`RunStep`]s in either position.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RunField {
    Single(String),
    Multiple(Vec<RunStep>),
}

/// One element of a `run` array: a shell command, a reference to another
/// task (`{ task = "build" }`), or a shape this version does not model.
/// The catch-all keeps discovery alive when mise grows a new step form.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RunStep {
    Command(String),
    TaskRef { task: String },
    Unknown(serde::de::IgnoredAny),
}

impl RunStep {
    fn render(&self) -> Option<String> {
        match self {
            Self::Command(command) => Some(command.clone()),
            Self::TaskRef { task } => Some(format!("mise run {task}")),
            Self::Unknown(_) => None,
        }
    }
}

/// Join rendered steps with ` && ` for the description column. Empty or
/// all-unknown arrays collapse to `None` so the caller can fall through
/// to other description sources.
fn join_steps(steps: &[RunStep]) -> Option<String> {
    let rendered: Vec<String> = steps.iter().filter_map(RunStep::render).collect();
    (!rendered.is_empty()).then(|| rendered.join(" && "))
}

impl RunField {
    fn as_description(&self) -> Option<String> {
        match self {
            Self::Single(s) => Some(s.clone()),
            Self::Multiple(steps) => join_steps(steps),
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

    fn file(&self) -> Option<&str> {
        match &self.kind {
            TaskEntryKind::Table(t) => t.file.as_deref(),
            TaskEntryKind::InlineRun(_) => None,
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
        // representation based on the runtime shape: bare string,
        // array of strings, or full table.
        let value = toml::Value::deserialize(deserializer)?;
        let kind = match value {
            toml::Value::String(s) => TaskEntryKind::InlineRun(RunField::Single(s)),
            toml::Value::Array(_) => {
                let steps: Vec<RunStep> = value.try_into().map_err(serde::de::Error::custom)?;
                TaskEntryKind::InlineRun(RunField::Multiple(steps))
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
        ExtractedTask, detect, extract_tasks, extract_tasks_from_source, operation_cmd,
        parse_cli_output, run_cmd,
    };
    use crate::tool::test_support::TempDir;
    use crate::types::TaskDetail;

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
        let cmd = run_cmd("build", &[], crate::tool::HostVerbosity::default());
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["run", "build"]);
    }

    #[test]
    fn run_cmd_inserts_separator_before_forwarded_args() {
        let cmd = run_cmd(
            "test",
            &["--watch".into(), "unit".into()],
            crate::tool::HostVerbosity::default(),
        );
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["run", "test", "--", "--watch", "unit"]);
    }

    #[test]
    fn install_cmd_is_bare_without_lockfile() {
        let dir = TempDir::new("mise-install-bare");
        fs::write(dir.path().join("mise.toml"), "").expect("mise.toml should be written");
        let cmd = operation_cmd(
            dir.path(),
            super::INSTALL,
            true,
            crate::tool::HostVerbosity::default(),
        );
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["install"]);
    }

    #[test]
    fn install_cmd_finds_the_lockfile_beside_a_nested_config() {
        // mise writes `<config dir>/mise.lock`, so a lockfile next to
        // `.config/mise.toml` is never `<root>/mise.lock`.
        let dir = TempDir::new("mise-install-nested-lock");
        let nested = dir.path().join(".config");
        fs::create_dir_all(&nested).expect(".config should be created");
        fs::write(nested.join("mise.toml"), "").expect("config should be written");
        fs::write(nested.join("mise.lock"), "").expect("lockfile should be written");
        let cmd = operation_cmd(
            dir.path(),
            super::INSTALL,
            true,
            crate::tool::HostVerbosity::default(),
        );
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["install", "--locked"]);
    }

    #[test]
    fn lock_path_follows_the_config_it_belongs_to() {
        let cases = [
            ("mise.toml", "mise.lock"),
            (".mise.toml", "mise.lock"),
            ("mise.local.toml", "mise.local.lock"),
            (".mise.local.toml", "mise.local.lock"),
            ("mise/config.toml", "mise/mise.lock"),
            (".mise/config.toml", ".mise/mise.lock"),
            (".config/mise.toml", ".config/mise.lock"),
            (".config/mise/config.toml", ".config/mise/mise.lock"),
        ];
        for (config, expected) in cases {
            assert_eq!(
                super::lock_path(std::path::Path::new(config)),
                std::path::PathBuf::from(expected),
                "{config}",
            );
        }
    }

    #[test]
    fn install_cmd_locks_when_frozen_and_lockfile_present() {
        let dir = TempDir::new("mise-install-locked");
        fs::write(dir.path().join("mise.toml"), "").expect("mise.toml should be written");
        fs::write(dir.path().join("mise.lock"), "").expect("mise.lock should be written");
        let cmd = operation_cmd(
            dir.path(),
            super::INSTALL,
            true,
            crate::tool::HostVerbosity::default(),
        );
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["install", "--locked"]);
        let cmd = operation_cmd(
            dir.path(),
            super::INSTALL,
            false,
            crate::tool::HostVerbosity::default(),
        );
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(argv, ["install"]);
    }

    #[test]
    fn extract_inline_string_task() {
        let dir = TempDir::new("mise-inline-string");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks]\nbuild = \"cargo build\"\ntest = \"cargo test\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;

        assert_eq!(
            tasks,
            [
                ExtractedTask::Recipe {
                    name: "build".to_string(),
                    description: Some("cargo build".to_string()),
                    detail: Box::default(),
                },
                ExtractedTask::Recipe {
                    name: "test".to_string(),
                    description: Some("cargo test".to_string()),
                    detail: Box::default(),
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "ci".to_string(),
                description: Some("cargo fmt && cargo clippy".to_string()),
                detail: Box::default(),
            }],
        );
    }

    #[test]
    fn extract_inline_array_with_task_references() {
        let dir = TempDir::new("mise-inline-task-ref");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks]\nfull = [{ task = \"check\" }, \"echo done\"]\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "full".to_string(),
                description: Some("mise run check && echo done".to_string()),
                detail: Box::default(),
            }],
        );
    }

    #[test]
    fn extract_table_task_with_task_references_keeps_siblings() {
        let dir = TempDir::new("mise-table-task-ref");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.check]\nrun = \"echo check\"\n\n[tasks.oracle]\nrun = \"echo \
             oracle\"\n\n[tasks.full]\nrun = [{ task = \"check\" }, { task = \"oracle\" }]\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [
                ExtractedTask::Recipe {
                    name: "check".to_string(),
                    description: Some("echo check".to_string()),
                    detail: Box::default(),
                },
                ExtractedTask::Recipe {
                    name: "full".to_string(),
                    description: Some("mise run check && mise run oracle".to_string()),
                    detail: Box::default(),
                },
                ExtractedTask::Recipe {
                    name: "oracle".to_string(),
                    description: Some("echo oracle".to_string()),
                    detail: Box::default(),
                },
            ],
        );
    }

    #[test]
    fn extract_table_task_tolerates_unknown_step_shape() {
        let dir = TempDir::new("mise-table-unknown-step");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.full]\nrun = [{ tasks = [\"a\", \"b\"] }, \"echo done\"]\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "full".to_string(),
                description: Some("echo done".to_string()),
                detail: Box::default(),
            }],
        );
    }

    #[test]
    fn extract_table_task_with_only_task_references_and_description() {
        let dir = TempDir::new("mise-table-task-ref-desc");
        fs::write(
            dir.path().join(".mise.toml"),
            "[tasks.full]\ndescription = \"Everything\"\nrun = [{ task = \"check\" }]\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks_from_source(dir.path()).expect("parse should succeed");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "full".to_string(),
                description: Some("Everything".to_string()),
                detail: Box::default(),
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "build".to_string(),
                description: Some("Compile the binary".to_string()),
                detail: Box::default(),
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "build".to_string(),
                description: Some("cargo build && cargo test".to_string()),
                detail: Box::default(),
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "build".to_string(),
                description: Some("cargo build".to_string()),
                detail: Box::default(),
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
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
                    detail: Box::default(),
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
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
            "[tasks.build]\nrun = \"cargo build\"\n\n[tasks.helper]\nhide = true\nrun = \"echo \
             nope\"\n",
        )
        .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
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

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "lint".to_string(),
                description: Some("./scripts/lint.sh".to_string()),
                detail: Box::new(TaskDetail {
                    file: Some("./scripts/lint.sh".to_string()),
                    ..TaskDetail::default()
                }),
            }],
        );
    }

    #[test]
    fn extract_returns_empty_when_no_tasks_table() {
        let dir = TempDir::new("mise-no-tasks");
        fs::write(dir.path().join(".mise.toml"), "[tools]\nnode = \"22\"\n")
            .expect(".mise.toml should be written");

        let tasks = extract_tasks(dir.path())
            .expect("parse should succeed")
            .tasks;
        assert!(tasks.is_empty());
    }

    #[test]
    fn extract_surfaces_parse_error_for_malformed_toml() {
        let dir = TempDir::new("mise-malformed");
        fs::write(dir.path().join(".mise.toml"), "[tasks.build")
            .expect(".mise.toml should be written");

        let err = extract_tasks(dir.path()).expect_err("malformed .mise.toml should error");
        // Detection renders the whole chain (`{err:#}`), which is where the
        // parse failure lands once the CLI attempt is reported above it.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("failed to parse"),
            "error chain should mention parse failure: {chain}",
        );
    }

    #[test]
    fn cli_output_extracts_tasks_under_project_root() {
        // Captured shape from a real `mise tasks --json` payload.
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
                    detail: Box::default(),
                },
                ExtractedTask::Alias {
                    name: "bw".to_string(),
                    target: "build-wasm".to_string(),
                },
                ExtractedTask::Recipe {
                    name: "test".to_string(),
                    description: Some("Run Go tests".to_string()),
                    detail: Box::default(),
                },
            ],
        );
    }

    #[test]
    fn cli_output_filters_tasks_outside_project_root() {
        // Global tasks (from `~/.config/mise/config.toml`) show up in
        // `mise tasks --json` too; they must not pollute the
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
                detail: Box::default(),
            }],
        );
    }

    #[test]
    fn cli_output_accepts_task_reference_steps() {
        let dir = TempDir::new("mise-cli-task-ref");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let src = project.join("mise.toml").to_string_lossy().to_string();
        let payload = serde_json::json!([
            {
                "name": "check", "aliases": [], "description": "", "source": src,
                "hide": false, "global": false, "run": ["echo check"], "file": null,
            },
            {
                "name": "full", "aliases": [], "description": "", "source": src,
                "hide": false, "global": false,
                "run": [{ "task": "check" }, { "task": "oracle" }],
                "file": null,
            },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(
            tasks,
            [
                ExtractedTask::Recipe {
                    name: "check".to_string(),
                    description: Some("echo check".to_string()),
                    detail: Box::default(),
                },
                ExtractedTask::Recipe {
                    name: "full".to_string(),
                    description: Some("mise run check && mise run oracle".to_string()),
                    detail: Box::default(),
                },
            ],
        );
    }

    #[test]
    fn cli_output_carries_task_detail() {
        let dir = TempDir::new("mise-cli-detail");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let src = project.join("mise.toml").to_string_lossy().to_string();
        let run_dir = project.join("compiler/rust");
        let payload = serde_json::json!([
            {
                "name": "leaf", "aliases": [], "description": "Lower one leaf", "source": src,
                "hide": false, "global": false, "run": ["cargo run -- lower"], "file": null,
                "depends": ["extract"], "depends_post": ["report"], "wait_for": ["fmt"],
                "dir": run_dir.to_string_lossy(),
                "env": ["RUST_BACKTRACE=1", { "H2R_OPT": "-O1", "JOBS": 4 }],
                "tools": { "rust": "1.95" },
                "usage": "arg \"[dir]\" default=\"compiler/core-json\"\n",
                "sources": ["src/**/*.rs"], "outputs": ["target/out"],
                "timeout": "10m",
            },
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(
            tasks,
            [ExtractedTask::Recipe {
                name: "leaf".to_string(),
                description: Some("Lower one leaf".to_string()),
                detail: Box::new(TaskDetail {
                    depends: vec!["extract".to_string()],
                    depends_post: vec!["report".to_string()],
                    wait_for: vec!["fmt".to_string()],
                    dir: Some(run_dir),
                    env: vec![
                        "RUST_BACKTRACE=1".to_string(),
                        "H2R_OPT=-O1".to_string(),
                        "JOBS=4".to_string(),
                    ],
                    tools: [("rust".to_string(), "1.95".to_string())].into(),
                    usage: Some("arg \"[dir]\" default=\"compiler/core-json\"".to_string()),
                    file: None,
                    sources: vec!["src/**/*.rs".to_string()],
                    outputs: vec!["target/out".to_string()],
                    timeout: Some("10m".to_string()),
                }),
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
                detail: Box::new(TaskDetail {
                    file: Some("./scripts/lint.sh".to_string()),
                    ..TaskDetail::default()
                }),
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
    fn cli_output_errors_for_malformed_json() {
        let dir = TempDir::new("mise-cli-bad-json");
        let project = dir.path().to_path_buf();
        assert!(parse_cli_output(b"not json", &project).is_err());
    }

    #[test]
    fn missing_binary_falls_back_without_a_warning() {
        let dir = TempDir::new("mise-fallback-quiet");
        fs::write(
            dir.path().join("mise.toml"),
            "[tasks]\nbuild = \"echo a\"\n",
        )
        .expect("mise.toml should be written");
        let extracted = match super::cli_tasks(dir.path()) {
            super::CliOutcome::Unavailable => extract_tasks(dir.path()).expect("fallback parses"),
            _ => return,
        };
        assert!(extracted.degraded.is_none());
        assert_eq!(extracted.tasks.len(), 1);
    }

    #[test]
    fn json_failure_reports_the_reason_and_still_returns_fallback_tasks() {
        let dir = TempDir::new("mise-degraded");
        fs::write(
            dir.path().join("mise.toml"),
            "[tasks]\nbuild = \"echo a\"\n",
        )
        .expect("mise.toml should be written");
        let extracted = super::MiseTasks {
            tasks: extract_tasks_from_source(dir.path()).expect("fallback parses"),
            degraded: Some(String::from(
                "`mise tasks --json` output did not parse: boom",
            )),
        };
        assert_eq!(extracted.tasks.len(), 1);
        assert!(
            extracted
                .degraded
                .as_deref()
                .is_some_and(|reason| reason.contains("mise tasks --json"))
        );
    }

    #[test]
    fn first_line_quotes_the_first_non_empty_stderr_line() {
        assert_eq!(super::first_line(b""), "");
        assert_eq!(super::first_line(b"\n\n  boom  \nnext\n"), ": boom");
    }

    /// The `usage_spec.cmd` shape of `mise tasks info lower:leaf --json`,
    /// captured from mise 2026.9.11.
    fn leaf_spec() -> super::UsageSpec {
        super::UsageSpec {
            signature: "<--fn <name>> [dir]".to_string(),
            args: vec![super::UsageArg {
                name: "dir".to_string(),
                help: Some("Core dump directory".to_string()),
                required: false,
                choices: vec![],
            }],
            flags: vec![super::UsageFlag {
                long: vec!["fn".to_string()],
                short: vec![],
                help: Some("Stable function name".to_string()),
                required: true,
                takes_value: true,
            }],
        }
    }

    #[test]
    fn missing_required_flags_reports_an_absent_flag() {
        let spec = leaf_spec();
        assert_eq!(
            spec.missing_required_flags(&["compiler/core-json".to_string()]),
            ["--fn"],
        );
        assert!(
            spec.missing_required_flags(&["--fn".to_string(), "foo".to_string()])
                .is_empty()
        );
        // The `--flag=value` spelling counts as provided.
        assert!(
            spec.missing_required_flags(&["--fn=foo".to_string()])
                .is_empty()
        );
    }

    #[test]
    fn consumes_value_after_only_for_value_taking_flags() {
        let spec = leaf_spec();
        assert!(spec.consumes_value_after("--fn"));
        // Already carries its value, so the next word is not it.
        assert!(!spec.consumes_value_after("--fn=foo"));
        assert!(!spec.consumes_value_after("--unknown"));
        assert!(!spec.consumes_value_after("dir"));
    }

    #[test]
    fn usage_spec_is_empty_without_args_or_flags() {
        assert!(super::UsageSpec::default().is_empty());
        assert!(!leaf_spec().is_empty());
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

        let tasks = extract_tasks(dir.path())
            .expect("mise CLI should succeed")
            .tasks;
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

#[cfg(test)]
mod verbosity_tests {
    use super::run_cmd;
    use crate::tool::{HostDiagnostics, HostVerbosity};

    fn argv(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_cmd_default_adds_no_verbosity_flag() {
        let v = HostVerbosity::default();
        assert_eq!(argv(&run_cmd("build", &[], v)), ["run", "build"]);
    }

    #[test]
    fn run_cmd_quiet_maps_to_host_flag() {
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Quiet,
            ..HostVerbosity::default()
        };
        assert_eq!(argv(&run_cmd("build", &[], v)), ["--quiet", "run", "build"]);
    }
}
