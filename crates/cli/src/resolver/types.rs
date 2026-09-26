//! The resolved settings of one invocation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::chain::FailurePolicy;
use crate::config::Download;
use crate::provider::Named;
use crate::tool::{
    HostVerbosity, OutputChoice, QuietLevel, Resolved, RunnerOutput, RunnerOutputPolicy, TaskStream,
};
use runner_core::ProviderId;

/// Every setting of one invocation, each from the highest layer that set it:
/// the command line, then `RUNNER_*` variables, then the task's `runner.toml`
/// table, then the project's, then the default.
#[derive(Debug, Clone)]
pub(crate) struct ResolutionOverrides {
    /// `--pm`, for the ecosystem the named package manager belongs to.
    pub pm: Option<PmOverride>,
    /// `--package`: the npm package whose declared binary the task token names.
    pub package: Option<String>,
    /// `--source`: the task source that must supply the task.
    pub source: Option<SourceOverride>,
    /// `--runtime` or `[runtime].javascript`.
    pub runtime: Option<RuntimeOverride>,
    /// The `-q` count the invocation asked for, inherited by nested runners.
    pub quiet_level: QuietLevel,
    /// What runner, tools and tasks print.
    pub output: Output,
    /// `--dry-run`: print the plan instead of running it.
    pub dry_run: bool,
    /// What a chain does after a task fails.
    pub failure_policy: FailurePolicy,
    /// Whether an install runs lifecycle scripts.
    pub script_policy: ScriptPolicy,
    /// Whether an install may rewrite the lockfile.
    pub lockfile: LockfilePolicy,
    /// Whether `runner install` installs detected toolchains first.
    pub install_tools: bool,
    /// Whether a command that downloads may run.
    pub download: DownloadPolicy,
    /// `[env]`, `[tools.*].env` and `[tasks.*].env`.
    pub env: EnvLayers,
    /// Per-task settings from `runner.toml`, keyed as written.
    pub tasks: BTreeMap<String, TaskChoice>,
    /// The `runner.toml` the settings came from.
    pub config: Option<PathBuf>,
    /// What a parent `runner`/`run` process already did above this one.
    pub parent: ParentMarkers,
}

impl Default for ResolutionOverrides {
    fn default() -> Self {
        Self {
            pm: None,
            package: None,
            source: None,
            runtime: None,
            quiet_level: QuietLevel::Off,
            output: Output::default(),
            dry_run: false,
            failure_policy: FailurePolicy::default(),
            script_policy: ScriptPolicy::Default,
            lockfile: LockfilePolicy::Update,
            install_tools: true,
            download: DownloadPolicy::default(),
            env: EnvLayers::default(),
            tasks: BTreeMap::new(),
            config: None,
            parent: ParentMarkers::default(),
        }
    }
}

/// The download setting and whether a layer set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DownloadPolicy {
    pub value: Download,
    /// `false` when no layer set it and the interactive default applies.
    pub explicit: bool,
}

impl Default for DownloadPolicy {
    fn default() -> Self {
        Self {
            value: Download::Allow,
            explicit: false,
        }
    }
}

/// The output layers that apply to every task, and the parallel buffering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Output {
    /// The command line over the environment.
    pub invocation: OutputChoice,
    /// `[output]`.
    pub project: OutputChoice,
    /// `[output.parallel].buffer`.
    pub buffer: Option<bool>,
}

/// One `[tasks.<name>]` table, parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskChoice {
    pub source: Option<ProviderId>,
    pub pm: Option<ProviderId>,
    pub runtime: Option<ProviderId>,
    pub output: OutputChoice,
}

impl TaskChoice {
    /// `self` where it sets a value, `lower` elsewhere.
    #[must_use]
    fn over(&self, lower: &Self) -> Self {
        Self {
            source: self.source.or(lower.source),
            pm: self.pm.or(lower.pm),
            runtime: self.runtime.or(lower.runtime),
            output: self.output.over(lower.output),
        }
    }
}

/// Whether an install may rewrite the lockfile.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LockfilePolicy {
    #[default]
    Update,
    Frozen,
}

/// Env markers a parent `runner`/`run` leaves for a nested one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ParentMarkers {
    /// A parent opened a GitHub Actions log group above this process.
    pub group_open: bool,
    /// A parent already printed this project's detection warnings.
    pub warned: bool,
}

impl ResolutionOverrides {
    /// The output for `task`, or for the invocation as a whole with `None`.
    pub(crate) fn output_for(&self, task: Option<&str>) -> Resolved {
        let task = task.map(|key| self.task(key).output).unwrap_or_default();
        self.output
            .invocation
            .over(task)
            .over(self.output.project)
            .resolve()
    }

    fn shows(&self, output: RunnerOutput) -> bool {
        self.output_for(None).runner.shows(output)
    }

    pub(crate) fn silences_warnings(&self) -> bool {
        !self.shows_warnings()
    }

    pub(crate) fn shows_progress(&self) -> bool {
        self.shows(RunnerOutput::Progress)
    }

    pub(crate) fn shows_warnings(&self) -> bool {
        self.shows(RunnerOutput::Warnings)
    }

    pub(crate) fn shows_errors(&self) -> bool {
        self.shows(RunnerOutput::Errors)
    }

    pub(crate) fn emits_groups(&self) -> bool {
        self.shows(RunnerOutput::Groups)
    }

    pub(crate) fn shows_summary(&self) -> bool {
        self.shows(RunnerOutput::Summary)
    }

    pub(crate) fn shows_fatal_errors(&self) -> bool {
        self.shows(RunnerOutput::FatalErrors)
    }

    /// Whether parallel output is held per task: `[output.parallel].buffer`,
    /// else under GitHub Actions while groups are on.
    pub(crate) fn buffers_parallel(&self, in_github_actions: bool) -> bool {
        self.output
            .buffer
            .unwrap_or_else(|| in_github_actions && self.emits_groups())
    }

    pub(crate) fn host_verbosity_for(&self, task: &str) -> HostVerbosity {
        HostVerbosity {
            diagnostics: self.output_for(Some(task)).tool,
        }
    }

    pub(crate) fn runner_output_for(&self, task: Option<&str>) -> RunnerOutputPolicy {
        self.output_for(task).runner
    }

    pub(crate) fn shows_progress_for(&self, task: &str) -> bool {
        self.runner_output_for(Some(task))
            .shows(RunnerOutput::Progress)
    }

    pub(crate) fn emits_groups_for(&self, task: &str) -> bool {
        self.runner_output_for(Some(task))
            .shows(RunnerOutput::Groups)
    }

    pub(crate) fn shows_timing_for(&self, task: &str) -> bool {
        self.runner_output_for(Some(task))
            .shows(RunnerOutput::Timing)
    }

    pub(crate) fn task_streams_for(&self, task: &str) -> (TaskStream, TaskStream) {
        let resolved = self.output_for(Some(task));
        (resolved.stdout, resolved.stderr)
    }

    /// The `[tasks]` settings for `key`, merged from the less specific
    /// spellings of the same task, least specific first: bare name,
    /// `source:name`, `member:name`, `scope:source#name`, then `key` itself.
    pub(crate) fn task(&self, key: &str) -> TaskChoice {
        spellings(key)
            .iter()
            .filter_map(|spelling| self.tasks.get(spelling))
            .fold(TaskChoice::default(), |merged, table| table.over(&merged))
    }

    /// The most specific `[tasks]` spelling of `key` whose table sets what
    /// `sets` asks about.
    pub(crate) fn table_for(&self, key: &str, sets: impl Fn(&TaskChoice) -> bool) -> String {
        spellings(key)
            .into_iter()
            .rev()
            .find(|spelling| self.tasks.get(spelling).is_some_and(&sets))
            .unwrap_or_else(|| key.to_owned())
    }

    /// The JS runtime for `key`: the command line or environment, then the
    /// task's table, then `[runtime]`.
    pub(crate) fn runtime_for(&self, key: &str) -> Option<RuntimeOverride> {
        let explicit = self
            .runtime
            .as_ref()
            .filter(|over| over.origin.propagates_to_nested());
        if let Some(over) = explicit {
            return Some(over.clone());
        }
        if let (Some(runtime), Some(path)) = (self.task(key).runtime, self.config.as_ref()) {
            return Some(RuntimeOverride {
                runtime,
                origin: OverrideOrigin::TaskConfig {
                    path: path.clone(),
                    task: self.table_for(key, |task| task.runtime.is_some()),
                },
            });
        }
        self.runtime.clone()
    }

    /// The JS runtime an explicit override selected for the invocation.
    pub(crate) fn js_runtime(&self) -> Option<ProviderId> {
        self.runtime.as_ref().map(|over| over.runtime)
    }
}

/// The `[tasks]` spellings of `key`, least specific first: bare name,
/// `source:name`, `member:name`, `scope:source#name`, then `key` itself.
fn spellings(key: &str) -> Vec<String> {
    let identity = TaskIdentity::parse(key);
    [
        identity.as_ref().map(|id| id.name.to_owned()),
        identity
            .as_ref()
            .map(|id| format!("{}:{}", id.source.label(), id.name)),
        identity
            .as_ref()
            .and_then(|id| (id.scope != "root").then(|| format!("{}:{}", id.scope, id.name))),
        identity
            .as_ref()
            .map(|id| crate::schema::labels::fqn_of(id.scope, id.source, id.name)),
        Some(key.to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// A qualified task identity: `source:name` (root scope) or
/// `scope:source#name`.
struct TaskIdentity<'a> {
    scope: &'a str,
    source: ProviderId,
    name: &'a str,
}

impl<'a> TaskIdentity<'a> {
    fn parse(task: &'a str) -> Option<Self> {
        if let Some((prefix, name)) = task.split_once('#') {
            let (scope, source) = prefix.rsplit_once(':').unwrap_or(("root", prefix));
            return crate::provider::task_source(source).map(|source| Self {
                scope,
                source,
                name,
            });
        }
        let (source, name) = task.split_once(':')?;
        crate::provider::task_source(source).map(|source| Self {
            scope: "root",
            source,
            name,
        })
    }
}

/// Whether an install runs lifecycle scripts.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ScriptPolicy {
    /// Each package manager's own default.
    #[default]
    Default,
    /// Skip them where the package manager can.
    Deny,
    /// Run them where the package manager can be told to.
    Allow,
}

impl ScriptPolicy {
    pub(crate) const fn from_setting(run: bool) -> Self {
        if run { Self::Allow } else { Self::Deny }
    }
}

/// The three environment layers a spawned process sees, narrowest last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EnvLayers {
    /// `[env]`.
    pub project: BTreeMap<String, String>,
    /// `[tools.<name>].env`, keyed by tool label.
    pub tool: BTreeMap<String, BTreeMap<String, String>>,
    /// `[tasks.<name>].env`, keyed by task name.
    pub task: BTreeMap<String, BTreeMap<String, String>>,
}

/// A package-manager override plus where it came from.
#[derive(Debug, Clone)]
pub(crate) struct PmOverride {
    pub pm: ProviderId,
    pub origin: OverrideOrigin,
}

/// A JS-runtime override plus where it came from.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeOverride {
    pub runtime: ProviderId,
    pub origin: OverrideOrigin,
}

impl RuntimeOverride {
    /// `bun via --runtime (command line)`.
    pub(crate) fn describe(&self) -> String {
        format!(
            "{} {}",
            self.runtime.label(),
            self.origin.describe("runtime")
        )
    }
}

/// A task-source selection plus where it came from.
#[derive(Debug, Clone)]
pub(crate) struct SourceOverride {
    pub source: ProviderId,
    pub origin: OverrideOrigin,
}

/// Where a setting came from, highest precedence first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverrideOrigin {
    /// A command-line flag.
    CliFlag,
    /// A `RUNNER_*` variable.
    EnvVar,
    /// A `[tasks.<task>]` table.
    TaskConfig { path: PathBuf, task: String },
    /// A project-wide `runner.toml` table.
    ConfigFile { path: PathBuf },
}

impl OverrideOrigin {
    /// Whether a nested `runner`/`run` inherits the value: a flag or a
    /// variable belongs to the invocation, a `runner.toml` value to its repo.
    pub(crate) const fn propagates_to_nested(&self) -> bool {
        matches!(self, Self::CliFlag | Self::EnvVar)
    }

    pub(crate) const fn from(origin: crate::invocation::Origin) -> Self {
        match origin {
            crate::invocation::Origin::Cli => Self::CliFlag,
            crate::invocation::Origin::Env => Self::EnvVar,
        }
    }

    /// `via --<flag> (command line)`, `via RUNNER_<FLAG> (environment)`, or
    /// `via runner.toml at <path>`.
    pub(crate) fn describe(&self, flag: &str) -> String {
        match self {
            Self::CliFlag => format!("via --{flag} (command line)"),
            Self::EnvVar => format!(
                "via {}_{} (environment)",
                crate::invocation::PREFIX,
                flag.replace('-', "_").to_uppercase()
            ),
            Self::TaskConfig { path, task } => {
                format!("via [tasks.{task}] in {}", path.display())
            }
            Self::ConfigFile { path } => format!("via runner.toml at {}", path.display()),
        }
    }

    pub(crate) fn describe_pm_source(&self) -> String {
        self.describe("pm")
    }
}
