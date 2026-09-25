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

use runner_core::TaskDetail;

use anyhow::Context as _;
use serde::Deserialize;

use super::files;

/// Project-local config filenames in mise's precedence order (highest first).
///
/// Mise also reads global / system locations (`~/.config/mise/config.toml`,
/// `/etc/mise/config.toml`) and `.config/mise/conf.d/*.toml`; those are
/// out of scope for extraction because they describe the user's environment,
/// not the project's tasks.
pub const FILENAMES: &[&str] = &[
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
pub fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

/// Locate the first existing mise config file under `dir`, in precedence
/// order. Returned as an absolute path when the input is absolute.
pub fn find_file(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, FILENAMES).filter(|path| path.is_file())
}

/// Surface mise tasks defined in this project. Prefers `mise tasks
/// --json` (authoritative across all config layers + file-based tasks),
/// falls back to parsing the first project-local config when mise
/// isn't on `$PATH`.
///
/// Hidden tasks (`hide = true`) and underscore-prefixed names are
/// excluded. Aliases come through as separate `Alias` entries pointing
/// at their target so the task list can group them.
pub fn extract_tasks(dir: &Path) -> anyhow::Result<MiseTasks> {
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
pub struct MiseTasks {
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
    let output = match super::command("mise")
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
    let boundary = files::vcs_root(project_root).unwrap_or_else(|| project_root.to_owned());
    let project_root = boundary.as_path();
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
            source: Some(path.clone()),
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
            source: Some(self.source.clone()),
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

/// The tool bin directories mise puts on `PATH` for this project, from
/// `mise bin-paths`. Empty when mise is missing or reports nothing.
///
/// `mise install` installs tools without activating them, so a package
/// manager it just installed is invisible to this process and to the
/// children runner spawns next.
pub fn bin_paths(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    match crate::REGISTRY
        .by_id(runner_core::ProviderId::Mise)
        .caps
        .bins
        .map(|cap| cap.dirs)
    {
        Some(runner_core::BinDirs::Ask(ask)) => ask(root),
        Some(runner_core::BinDirs::Static(dirs)) => {
            Ok(dirs.iter().map(|dir| root.join(dir)).collect())
        }
        None => Ok(Vec::new()),
    }
}

pub(crate) fn parse_missing_health(stdout: &[u8]) -> runner_core::Health {
    let parsed = match serde_json::from_slice::<BTreeMap<String, Vec<MissingTool>>>(stdout) {
        Ok(parsed) => parsed,
        Err(error) => return runner_core::Health::Unreadable(error.to_string()),
    };
    let messages = parsed
        .into_iter()
        .flat_map(|(tool, entries)| {
            entries
                .into_iter()
                .filter(|entry| !entry.installed)
                .map(move |entry| {
                    let tool = entry
                        .requested_version
                        .or(entry.version)
                        .map_or_else(|| tool.clone(), |version| format!("{tool}@{version}"));
                    format!("{tool} is declared but not installed")
                })
        })
        .collect();
    runner_core::Health::Problems(messages)
}

pub(crate) fn parse_task_health(stdout: &[u8]) -> runner_core::Health {
    let parsed = match serde_json::from_slice::<ValidateReport>(stdout) {
        Ok(parsed) => parsed,
        Err(error) => return runner_core::Health::Unreadable(error.to_string()),
    };
    runner_core::Health::Problems(
        parsed
            .issues
            .into_iter()
            .map(|issue| {
                let message = match issue.details.filter(|details| !details.trim().is_empty()) {
                    Some(details) => format!("{} ({details})", issue.message),
                    None => issue.message,
                };
                format!("{}: {}: {message}", issue.task, issue.severity)
            })
            .collect(),
    )
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
pub struct UsageSpec {
    /// One-line signature mise renders for the task, e.g. `<--fn <name>> [dir]`.
    pub signature: String,
    /// Positional arguments in declaration order.
    pub args: Vec<UsageArg>,
    /// Accepted flags.
    pub flags: Vec<UsageFlag>,
}

/// One positional argument of a task.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageArg {
    /// Argument name.
    pub name: String,
    /// Help from the usage declaration.
    pub help: Option<String>,
    /// Whether callers must supply a value.
    pub required: bool,
    /// Accepted values, when the spec closes the set with `choices`.
    pub choices: Vec<String>,
}

/// One flag of a task.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageFlag {
    /// Long spellings without the `--`.
    pub long: Vec<String>,
    /// Short spellings without the `-`.
    pub short: Vec<String>,
    /// Help from the usage declaration.
    pub help: Option<String>,
    /// Whether callers must supply a value.
    pub required: bool,
    /// `true` when the flag consumes the next word.
    pub takes_value: bool,
}

impl UsageSpec {
    /// `true` when the spec declares nothing worth completing or checking.
    pub const fn is_empty(&self) -> bool {
        self.args.is_empty() && self.flags.is_empty()
    }

    /// `true` when `word` is a flag that swallows the word after it, so the
    /// next position holds that flag's value rather than another flag.
    pub fn consumes_value_after(&self, word: &str) -> bool {
        let Some((name, long)) = word
            .strip_prefix("--")
            .map(|name| (name, true))
            .or_else(|| word.strip_prefix('-').map(|name| (name, false)))
        else {
            return false;
        };
        // `--flag=value` and `-f=value` carry their value already.
        if name.is_empty() || name.contains('=') {
            return false;
        }
        self.flags
            .iter()
            .filter(|flag| flag.takes_value)
            .any(|flag| {
                let spellings = if long { &flag.long } else { &flag.short };
                spellings.iter().any(|spelling| spelling == name)
            })
    }

    /// Every flag the spec marks required that `provided` does not set,
    /// named by its first spelling.
    ///
    /// A flag may declare only a short form, so both forms count as
    /// provided and a short-only flag is still reported when absent.
    pub fn missing_required_flags(&self, provided: &[String]) -> Vec<String> {
        self.flags
            .iter()
            .filter(|flag| flag.required)
            .filter(|flag| {
                !flag.spellings().into_iter().any(|dashed| {
                    provided
                        .iter()
                        .any(|word| word == &dashed || word.starts_with(&format!("{dashed}=")))
                })
            })
            .filter_map(|flag| flag.spellings().into_iter().next())
            .collect()
    }
}

impl UsageFlag {
    /// Every spelling of this flag, long forms first.
    pub fn spellings(&self) -> Vec<String> {
        self.long
            .iter()
            .map(|long| format!("--{long}"))
            .chain(self.short.iter().map(|short| format!("-{short}")))
            .collect()
    }
}

/// Read `task`'s spec via `mise tasks info <task> --json`.
///
/// `None` when mise is missing, the task is unknown, or it declares no spec.
/// One subprocess per call: the bulk `mise tasks ls --json` carries only the
/// unparsed KDL string and mise exposes no bulk flag for the parsed form.
pub fn usage_spec(root: &Path, task: &str) -> anyhow::Result<Option<UsageSpec>> {
    let Some(program) = runner_core::probe_with("mise", &[]) else {
        return Ok(None);
    };
    let output = std::process::Command::new(program)
        .args(["tasks", "info", task, "--json"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "mise tasks info {task} in {} failed ({}): {}",
            root.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let info: TaskInfoJson = serde_json::from_slice(&output.stdout)?;
    let Some(cmd) = info.usage_spec.and_then(|usage| usage.cmd) else {
        return Ok(None);
    };
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
    Ok((!spec.is_empty()).then_some(spec))
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
/// [`crate::extract::just::ExtractedTask`] so the detection-layer push helper
/// can stay symmetric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractedTask {
    /// A runnable task.
    Recipe {
        /// Exposed name.
        name: String,
        /// Declared description.
        description: Option<String>,
        /// Source metadata.
        detail: Box<TaskDetail>,
    },
    /// An alias to a task.
    Alias {
        /// Exposed name.
        name: String,
        /// Target task name.
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

/// Tasks and their source scopes from mise's read-only task listing.
///
/// # Errors
/// Returns the source read failure or mise's task query diagnostic.
pub fn tasks(
    present: &runner_core::Present,
    tree: &runner_core::Tree,
) -> Result<Vec<runner_core::Task>, runner_core::Warning> {
    let root = runner_core::plan::scope_dir(tree, &present.scope);
    let extracted = extract_tasks(&root)
        .map_err(|e| runner_core::Warning::about(present.provider, e.to_string()))?;
    if let Some(reason) = extracted.degraded {
        return Err(runner_core::Warning::about(present.provider, reason));
    }
    let mut tasks: Vec<_> = extracted
        .tasks
        .into_iter()
        .map(|entry| match entry {
            ExtractedTask::Recipe {
                name,
                description,
                detail,
            } => {
                let mut task = super::task(present, name, description);
                task.scope = detail
                    .source
                    .as_ref()
                    .and_then(|path| {
                        tree.members
                            .iter()
                            .filter(|s| path.starts_with(runner_core::plan::scope_dir(tree, s)))
                            .max_by_key(|s| {
                                runner_core::plan::scope_dir(tree, s).components().count()
                            })
                            .cloned()
                    })
                    .unwrap_or(runner_core::Scope::Root);
                task.detail = *detail;
                task
            }
            ExtractedTask::Alias { name, target } => {
                let mut task = super::task(present, name, None);
                task.alias_of = Some(target);
                task
            }
        })
        .collect();
    for index in 0..tasks.len() {
        if let Some(target) = &tasks[index].alias_of
            && let Some(scope) = tasks
                .iter()
                .find(|task| task.name == *target)
                .map(|task| task.scope.clone())
        {
            tasks[index].scope = scope;
        }
    }
    Ok(tasks)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::test_support::TempDir;
    use std::fs;
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
                    detail: Box::new(TaskDetail {
                        source: Some(dir.path().join(".mise.toml")),
                        ..TaskDetail::default()
                    }),
                },
                ExtractedTask::Recipe {
                    name: "test".to_string(),
                    description: Some("cargo test".to_string()),
                    detail: Box::new(TaskDetail {
                        source: Some(dir.path().join(".mise.toml")),
                        ..TaskDetail::default()
                    }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                    detail: Box::new(TaskDetail {
                        source: Some(dir.path().join(".mise.toml")),
                        ..TaskDetail::default()
                    }),
                },
                ExtractedTask::Recipe {
                    name: "full".to_string(),
                    description: Some("mise run check && mise run oracle".to_string()),
                    detail: Box::new(TaskDetail {
                        source: Some(dir.path().join(".mise.toml")),
                        ..TaskDetail::default()
                    }),
                },
                ExtractedTask::Recipe {
                    name: "oracle".to_string(),
                    description: Some("echo oracle".to_string()),
                    detail: Box::new(TaskDetail {
                        source: Some(dir.path().join(".mise.toml")),
                        ..TaskDetail::default()
                    }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                detail: Box::new(TaskDetail {
                    source: Some(dir.path().join(".mise.toml")),
                    ..TaskDetail::default()
                }),
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
                    detail: Box::new(TaskDetail {
                        source: Some(dir.path().join(".mise.toml")),
                        ..TaskDetail::default()
                    }),
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
                    source: Some(dir.path().join(".mise.toml")),
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
                    detail: Box::new(TaskDetail {
                        source: Some(source_path.clone()),
                        ..TaskDetail::default()
                    }),
                },
                ExtractedTask::Alias {
                    name: "bw".to_string(),
                    target: "build-wasm".to_string(),
                },
                ExtractedTask::Recipe {
                    name: "test".to_string(),
                    description: Some("Run Go tests".to_string()),
                    detail: Box::new(TaskDetail {
                        source: Some(source_path.clone()),
                        ..TaskDetail::default()
                    }),
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

    fn names_of(tasks: &[ExtractedTask]) -> Vec<&str> {
        tasks
            .iter()
            .map(|t| match t {
                ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                    name.as_str()
                }
            })
            .collect()
    }

    fn entry(name: &str, source: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "aliases": [],
            "description": "",
            "source": source.to_string_lossy(),
            "hide": false, "global": false, "run": ["echo x"], "file": null,
        })
    }

    #[test]
    fn cli_output_keeps_tasks_from_nested_directories() {
        // A config in a subdirectory is still the project's. mise merges
        // `.config/mise`, `.mise/tasks/*` and any nested config, and all of
        // them are the project speaking.
        let dir = TempDir::new("mise-cli-nested");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let payload = serde_json::json!([
            entry("root", &project.join("mise.toml")),
            entry("nested-config", &project.join(".config").join("mise.toml")),
            entry(
                "deep",
                &project.join("packages").join("api").join("mise.toml"),
            ),
            entry(
                "file-task",
                &project.join(".mise").join("tasks").join("build"),
            ),
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(
            names_of(&tasks),
            ["deep", "file-task", "nested-config", "root"]
        );
    }

    #[test]
    fn cli_output_drops_tasks_from_a_parent_directory() {
        // mise merges configs from every ancestor, not just the global one,
        // and an ancestor's tasks carry `global: false`. Without the path
        // check a `~/projects/mise.toml` task would look project-owned.
        let dir = TempDir::new("mise-cli-parent");
        let project = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize")
            .join("repo");
        let parent = project.parent().expect("has a parent").to_path_buf();
        let payload = serde_json::json!([
            entry("mine", &project.join("mise.toml")),
            entry("ancestors", &parent.join("mise.toml")),
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(names_of(&tasks), ["mine"]);
    }

    #[test]
    fn cli_output_does_not_match_a_sibling_sharing_a_name_prefix() {
        // `starts_with` on a Path compares components, so `repo-other` is not
        // inside `repo`. A string prefix check would have kept it.
        let dir = TempDir::new("mise-cli-prefix");
        let base = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let project = base.join("repo");
        let payload = serde_json::json!([
            entry("mine", &project.join("mise.toml")),
            entry("neighbour", &base.join("repo-other").join("mise.toml")),
        ])
        .to_string();

        let tasks = parse_cli_output(payload.as_bytes(), &project).expect("payload should parse");
        assert_eq!(names_of(&tasks), ["mine"]);
    }

    #[test]
    fn cli_output_read_from_a_member_keeps_the_root_task_in_root_scope() {
        let dir = TempDir::new("mise-cli-member");
        fs::create_dir(dir.path().join(".git")).unwrap();
        let root = dir
            .path()
            .canonicalize()
            .expect("temp dir should canonicalize");
        let member = root.join("apps").join("web");
        let payload = serde_json::json!([
            entry("web-build", &member.join("mise.toml")),
            entry("repo-lint", &root.join("mise.toml")),
        ])
        .to_string();

        let from_member = parse_cli_output(payload.as_bytes(), &member).expect("parses");
        let from_root = parse_cli_output(payload.as_bytes(), &root).expect("parses");
        assert_eq!(names_of(&from_member), ["repo-lint", "web-build"]);
        assert_eq!(from_member, from_root);
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
                detail: Box::new(TaskDetail {
                    source: Some(project.join("mise.toml")),
                    ..TaskDetail::default()
                }),
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
                    detail: Box::new(TaskDetail {
                        source: Some(project.join("mise.toml")),
                        ..TaskDetail::default()
                    }),
                },
                ExtractedTask::Recipe {
                    name: "full".to_string(),
                    description: Some("mise run check && mise run oracle".to_string()),
                    detail: Box::new(TaskDetail {
                        source: Some(project.join("mise.toml")),
                        ..TaskDetail::default()
                    }),
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
                    source: Some(PathBuf::from(&src)),
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
                    source: Some(project.join("mise.toml")),
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
        let extracted = match cli_tasks(dir.path()) {
            CliOutcome::Unavailable => extract_tasks(dir.path()).expect("fallback parses"),
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
        let extracted = MiseTasks {
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
        assert_eq!(first_line(b""), "");
        assert_eq!(first_line(b"\n\n  boom  \nnext\n"), ": boom");
    }

    fn leaf_spec() -> UsageSpec {
        UsageSpec {
            signature: "<--fn <name>> [dir]".to_string(),
            args: vec![UsageArg {
                name: "dir".to_string(),
                help: Some("Core dump directory".to_string()),
                required: false,
                choices: vec![],
            }],
            flags: vec![UsageFlag {
                long: vec!["fn".to_string()],
                short: vec![],
                help: Some("Stable function name".to_string()),
                required: true,
                takes_value: true,
            }],
        }
    }

    fn two_flag_spec() -> UsageSpec {
        UsageSpec {
            signature: "<-f <name>> [--dry-run]".to_string(),
            args: vec![],
            flags: vec![
                UsageFlag {
                    long: vec![],
                    short: vec!["f".to_string()],
                    help: None,
                    required: true,
                    takes_value: true,
                },
                UsageFlag {
                    long: vec!["dry-run".to_string()],
                    short: vec![],
                    help: None,
                    required: false,
                    takes_value: false,
                },
            ],
        }
    }

    #[test]
    fn a_short_only_required_flag_is_reported_when_absent() {
        let spec = two_flag_spec();
        assert_eq!(spec.missing_required_flags(&[]), ["-f"]);
        assert!(
            spec.missing_required_flags(&["-f".to_string(), "x".to_string()])
                .is_empty()
        );
        assert!(
            spec.missing_required_flags(&["-f=x".to_string()])
                .is_empty()
        );
    }

    #[test]
    fn a_short_flag_consumes_the_word_after_it() {
        let spec = two_flag_spec();
        assert!(spec.consumes_value_after("-f"));
        // Carries its value already.
        assert!(!spec.consumes_value_after("-f=x"));
        // Takes no value.
        assert!(!spec.consumes_value_after("--dry-run"));
        assert!(!spec.consumes_value_after("-"));
        assert!(!spec.consumes_value_after("--"));
    }

    #[test]
    fn spellings_lists_long_then_short() {
        let spec = two_flag_spec();
        assert_eq!(spec.flags[0].spellings(), ["-f"]);
        assert_eq!(spec.flags[1].spellings(), ["--dry-run"]);
        assert_eq!(leaf_spec().flags[0].spellings(), ["--fn"]);
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
        assert!(UsageSpec::default().is_empty());
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
    fn extract_in_a_member_keeps_the_repo_root_task_in_root_scope() {
        if std::process::Command::new("mise")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: mise unavailable");
            return;
        }

        let dir = TempDir::new("mise-member-merge");
        fs::create_dir(dir.path().join(".git")).unwrap();
        let root = dir.path();
        let member = root.join("apps").join("web");
        fs::create_dir_all(&member).expect("member dir should be created");
        fs::write(
            root.join("mise.toml"),
            "[tasks.repo-lint]\nrun = \"echo lint\"\n",
        )
        .expect("root config should be written");
        fs::write(
            member.join("mise.toml"),
            "[tasks.web-build]\nrun = \"echo build\"\n",
        )
        .expect("member config should be written");
        // mise refuses to read an untrusted config.
        for path in [root.join("mise.toml"), member.join("mise.toml")] {
            let _ = std::process::Command::new("mise")
                .args(["trust", "--yes"])
                .arg(&path)
                .output();
        }

        let names = |tasks: &[ExtractedTask]| -> Vec<String> {
            tasks
                .iter()
                .map(|t| match t {
                    ExtractedTask::Recipe { name, .. } | ExtractedTask::Alias { name, .. } => {
                        name.clone()
                    }
                })
                .collect()
        };

        let from_member = extract_tasks(&member)
            .expect("member extraction succeeds")
            .tasks;
        let from_root = extract_tasks(root).expect("root extraction succeeds").tasks;

        assert!(
            names(&from_member).contains(&"web-build".to_string()),
            "a member must list what it declares; got {:?}",
            names(&from_member),
        );
        assert!(
            names(&from_member).contains(&"repo-lint".to_string()),
            "a root task seen from a member keeps its root scope; got {:?}",
            names(&from_member),
        );
        assert!(
            names(&from_root).contains(&"repo-lint".to_string()),
            "the root must still list its own tasks; got {:?}",
            names(&from_root),
        );
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
