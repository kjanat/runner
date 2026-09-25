//! Override data types, the public structs/enums + their trivial impls.
//!
//! No parsing, just the shapes the rest of the module passes around.
//! `impl ResolutionOverrides` lives in [`super::overrides`].

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::chain::FailurePolicy;
use crate::config::LoadedConfig;
use crate::tool::{
    HostDiagnostics, HostVerbosity, OutputPolicy, QuietLevel, RunnerOutput, RunnerOutputPolicy,
    Stream, TaskStream,
};
use crate::types::{Ecosystem, JsRuntime, PackageManager, TaskRunner, TaskSource};

/// User-supplied overrides assembled from CLI flags, environment variables,
/// and (Phase 3+) a `runner.toml` file.
///
/// Each field carries an [`OverrideOrigin`] so diagnostic output (Phase 6)
/// can attribute a decision to the exact source the user set it from.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolutionOverrides {
    /// Cross-ecosystem PM override from CLI/env. `--pm`/`RUNNER_PM` are not
    /// ecosystem-qualified; the resolver applies this value only when the
    /// named PM is compatible with the requested ecosystem.
    pub pm: Option<PmOverride>,
    /// `--package`: the npm package whose declared binary the task token
    /// names. Resolution then reads that package's own manifest and, when it
    /// is not installed, the package manager's package-selection form.
    pub package: Option<String>,
    /// Per-ecosystem PM overrides from `runner.toml`. Consulted after the
    /// cross-ecosystem CLI/env override falls through (e.g. `--pm cargo`
    /// against a Node resolution).
    pub pm_by_ecosystem: HashMap<Ecosystem, PmOverride>,
    /// Task-runner override from `--runner` / `RUNNER_RUNNER`. When set,
    /// the source selector restricts candidates to that runner's
    /// [`TaskRunner::task_source`].
    pub runner: Option<RunnerOverride>,
    /// JS runtime override from `--runtime` / `RUNNER_RUNTIME` /
    /// `[runtime].js`. Selects which runtime executes a task's process tree
    /// (`bun --bun run` vs `npm run`) and which runtime runs a local
    /// `.ts`/`.js` file, an axis `--pm` conflated with "who installs here".
    pub runtime: Option<RuntimeOverride>,
    /// Ranked preference list from the **deprecated** `[task_runner].prefer`.
    /// Empty when no config is loaded, the section is empty, or `[tasks]`
    /// supersedes it. When non-empty, the source selector restricts candidates
    /// to runners in the list (in listed order).
    pub prefer_runners: Vec<TaskRunner>,
    /// Global rank-only task-source order from `[tasks].prefer`. Empty when
    /// unset. Each entry is a [`TaskSource`] (resolved from a runner, package
    /// manager, or source label); a same-name conflict prefers earlier
    /// entries, and any source not listed still resolves (it just ranks
    /// below listed ones). Never restricts.
    pub prefer_sources: Vec<TaskSource>,
    /// Per-task source pins from `[tasks].overrides`: task name → preferred
    /// [`TaskSource`]s, most-native first. When a pinned task has a candidate
    /// under one of these sources, that candidate wins; otherwise the normal
    /// ranking applies (no hard error).
    pub task_source_overrides: BTreeMap<String, Vec<TaskSource>>,
    /// What to do when no signal in steps 2–6 matches.
    pub fallback: FallbackPolicy,
    /// What to do when the manifest declaration (step 5) disagrees with
    /// the detected lockfile (step 6).
    pub on_mismatch: MismatchPolicy,
    /// When `true`, suppress all `DetectionWarning` output. Set via
    /// `--no-warnings` / `RUNNER_NO_WARNINGS`. Errors still surface;
    /// only non-fatal warnings are silenced.
    pub no_warnings: bool,
    /// Global quiet level from `-q` through `-qqqq` (repeat count) and
    /// `RUNNER_QUIET` (numeric `0..4`, clamped, or a truthy word → level 1). CLI > env: a
    /// passed `-q` count wins outright, env applies only when no flag was given.
    /// The level selects an explicit [`OutputPolicy`] preset; it is not itself
    /// used as a collection of category thresholds.
    pub quiet_level: QuietLevel,
    /// Whether the global host diagnostic axis was selected by a quiet preset
    /// or `[host].diagnostics`, including an explicit `normal` value.
    pub host_diagnostics_explicit: bool,
    /// Effective independent category policy after preset + config resolution.
    pub output_policy: OutputPolicy,
    /// Global stdout-clean intent from `--host-stream` / `RUNNER_HOST_STREAM`,
    /// when the invocation set one, `inherit` included. Orthogonal to
    /// [`Self::quiet_level`]: when [`Stream::Stderr`], hosts that can (pnpm)
    /// divert their diagnostics to stderr so a pipeline parsing stdout stays
    /// clean. Outranks per-task config.
    pub host_stream: Option<Stream>,
    /// Global `[host].stream` value; per-task config may override it.
    pub host_stream_config: Stream,
    /// Per-task verbosity partials from `[tasks.<name>].verbosity`, keyed by
    /// task name. Each is deep-merged (defu-like) under the global CLI/env
    /// level+stream at dispatch time by [`Self::host_verbosity_for`]; a partial
    /// only overrides the axis it names.
    pub task_verbosity: BTreeMap<String, TaskVerbosity>,
    /// When `true`, emit a one-line trace describing which chain step
    /// produced the PM decision. Set via `--explain` / `RUNNER_EXPLAIN`.
    pub explain: bool,
    /// Failure policy for `run -s/-p` chains and `runner install <tasks>`.
    /// Resolved from `-k`/`-K` (CLI) → `RUNNER_KEEP_GOING`/`RUNNER_KILL_ON_FAIL`
    /// (env) → `[chain]` (config) → `FailFast`.
    pub failure_policy: FailurePolicy,
    /// `[github]` and `[parallel]` output grouping.
    pub grouping: OutputGrouping,
    /// Install-time lifecycle-script policy, resolved from
    /// `RUNNER_INSTALL_SCRIPTS` (env) → `[install].scripts` (config). The CLI
    /// `--no-scripts` ([`ScriptPolicy::Deny`]) / `--scripts`
    /// ([`ScriptPolicy::Allow`]) flags are layered on top at the dispatch
    /// boundary; [`ScriptPolicy::Default`] leaves each package manager at its
    /// own built-in default.
    pub script_policy: ScriptPolicy,
    /// Install-directory collision policy, resolved from
    /// `RUNNER_INSTALL_ON_COLLISION` (env) → `[install].on_collision` (config).
    pub on_collision: CollisionPolicy,
    /// What a command that can download may do, resolved from `--fetch`
    /// (CLI) → `RUNNER_REACH` (env) → `[defaults].fetch` (config). `ask`
    /// prompts on a terminal, `allow` proceeds, `local` refuses.
    pub reach: runner_core::ReachPolicy,
    /// `[defaults].frozen`: installs keep the lockfile even without `--frozen`.
    pub lockfile: LockfilePolicy,
    /// `[env]`, `[tools.*].env` and `[tasks.*].env`, kept together because
    /// they are one layered lookup rather than three independent knobs.
    pub env: EnvLayers,
    /// `[tools.<name>].install`, normalized to an ordered operation list.
    /// Absent means the tool's default, which every tool spells `install`.
    pub tool_install: BTreeMap<String, Vec<String>>,
    /// What a parent `runner`/`run` process already did above this one.
    pub parent: ParentMarkers,
}

/// Whether task output is grouped into collapsible blocks.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[schemars(
    deny_unknown_fields,
    description = "Whether task output is grouped into collapsible blocks, under GitHub Actions \
                   and elsewhere."
)]
pub(crate) struct OutputGrouping {
    /// Broad GitHub Actions grouping switch (`[github].group_output`).
    pub group_output: bool,
    /// Group parallel output under GitHub Actions
    /// (`[github].group_parallel`).
    pub github_group_parallel: bool,
    /// Group parallel output outside GitHub Actions
    /// (`[parallel].grouped`).
    pub parallel_grouped: bool,
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
    /// `true` when a parent already opened a GitHub Actions log group above
    /// this process (the inherited `RUNNER_GROUP_ACTIVE` marker). A nested
    /// `::endgroup::` closes the parent's group early, so this runner's own
    /// group-opening sites stay silent.
    pub group_open: bool,
    /// `true` when a parent already emitted this project's detection
    /// warnings (the inherited `RUNNER_WARNED_ROOT` marker, which carries the
    /// root it warned about).
    pub warned: bool,
}

/// A per-task verbosity partial from `[tasks.<name>].verbosity`. Each axis is
/// optional so a config table that names only one knob leaves the other to be
/// inherited from the global (CLI/env) level/stream during the deep-merge in
/// [`ResolutionOverrides::host_verbosity_for`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TaskVerbosity {
    /// Per-task quiet level, if the table set one.
    pub level: Option<QuietLevel>,
    /// Per-task stream routing, if the table set one.
    pub stream: Option<Stream>,
    /// Explicit task stdout policy.
    pub stdout: Option<TaskStream>,
    /// Explicit task stderr policy.
    pub stderr: Option<TaskStream>,
    pub progress: Option<bool>,
    pub groups: Option<bool>,
    pub task_timing: Option<bool>,
}

impl ResolutionOverrides {
    /// `true` when non-fatal warnings should be muted: either `--no-warnings`
    /// was set explicitly, or the quiet level reached `-qq` (which folds in
    /// `--no-warnings`).
    pub(crate) const fn silences_warnings(&self) -> bool {
        !self.shows_warnings()
    }

    pub(crate) const fn shows_progress(&self) -> bool {
        self.output_policy.runner.shows(RunnerOutput::Progress)
    }

    pub(crate) const fn shows_warnings(&self) -> bool {
        self.output_policy.runner.shows(RunnerOutput::Warnings)
    }

    pub(crate) const fn shows_errors(&self) -> bool {
        self.output_policy.runner.shows(RunnerOutput::Errors)
    }

    pub(crate) const fn emits_groups(&self) -> bool {
        self.output_policy.runner.shows(RunnerOutput::Groups)
    }

    pub(crate) const fn shows_summary(&self) -> bool {
        self.output_policy.runner.shows(RunnerOutput::Summary)
    }

    pub(crate) const fn shows_fatal_errors(&self) -> bool {
        self.output_policy.runner.shows(RunnerOutput::FatalErrors)
    }

    /// Resolve the effective [`HostVerbosity`] for a task, deep-merging the
    /// global CLI/env level+stream over the task's `[tasks.<name>].verbosity`
    /// partial (defu-like, per axis). The global side wins where set; the
    /// per-task config fills each axis the global left at its default.
    pub(crate) fn host_verbosity_for(&self, task: &str) -> HostVerbosity {
        let per_task = self.task_verbosity_for(task);
        let diagnostics = if self.host_diagnostics_explicit {
            self.output_policy.host_diagnostics
        } else {
            per_task
                .level
                .map_or(HostDiagnostics::Normal, HostDiagnostics::from_legacy_quiet)
        };
        let stream = self
            .host_stream
            .unwrap_or_else(|| per_task.stream.unwrap_or(self.host_stream_config));
        HostVerbosity {
            diagnostics,
            stream,
        }
    }

    /// The host stream every task inherits unless its own config says otherwise.
    pub(crate) fn global_host_stream(&self) -> Stream {
        self.host_stream.unwrap_or(self.host_stream_config)
    }

    /// The runner output categories shown for `task`, or globally for `None`.
    pub(crate) fn runner_output_for(&self, task: Option<&str>) -> RunnerOutputPolicy {
        let Some(task) = task else {
            return self.output_policy.runner;
        };
        let per_task = self.task_verbosity_for(task);
        self.output_policy.runner.and(
            RunnerOutputPolicy::ALL
                .with(RunnerOutput::Progress, per_task.progress.unwrap_or(true))
                .with(RunnerOutput::Groups, per_task.groups.unwrap_or(true))
                .with(
                    RunnerOutput::TaskTiming,
                    per_task.task_timing.unwrap_or(true),
                ),
        )
    }

    pub(crate) fn shows_progress_for(&self, task: &str) -> bool {
        self.runner_output_for(Some(task))
            .shows(RunnerOutput::Progress)
    }

    pub(crate) fn emits_groups_for(&self, task: &str) -> bool {
        self.runner_output_for(Some(task))
            .shows(RunnerOutput::Groups)
    }

    pub(crate) fn shows_task_timing_for(&self, task: &str) -> bool {
        self.runner_output_for(Some(task))
            .shows(RunnerOutput::TaskTiming)
    }

    pub(crate) fn task_streams_for(&self, task: &str) -> (TaskStream, TaskStream) {
        let per_task = self.task_verbosity_for(task);
        (
            per_task.stdout.unwrap_or(TaskStream::Inherit),
            per_task.stderr.unwrap_or(TaskStream::Inherit),
        )
    }

    /// Resolve a task's settings by exact key, then fill missing axes from
    /// the less specific spellings of the same identity, least specific
    /// first: bare name, `source:name`, `member:name` (member tasks),
    /// `scope:source#name`, exact key.
    fn task_verbosity_for(&self, task: &str) -> TaskVerbosity {
        let identity = TaskIdentity::parse(task);
        let lookup = |key: Option<String>| {
            key.and_then(|key| self.task_verbosity.get(&key))
                .copied()
                .unwrap_or_default()
        };
        let base = lookup(identity.as_ref().map(|id| id.name.to_string()));
        let shorthand = lookup(
            identity
                .as_ref()
                .map(|id| format!("{}:{}", id.source.label(), id.name)),
        );
        let member = lookup(
            identity
                .as_ref()
                .and_then(|id| (id.scope != "root").then(|| format!("{}:{}", id.scope, id.name))),
        );
        let fqn = lookup(
            identity
                .as_ref()
                .map(|id| crate::schema::labels::fqn_of(id.scope, id.source, id.name)),
        );
        let exact = self.task_verbosity.get(task).copied().unwrap_or_default();
        [shorthand, member, fqn, exact]
            .into_iter()
            .fold(base, merge_task_verbosity)
    }

    /// The JS runtime an explicit override selected, if any. The single read
    /// of [`Self::runtime`]'s value; everything that dispatches, propagates or
    /// reports the runtime goes through here.
    pub(crate) fn js_runtime(&self) -> Option<JsRuntime> {
        self.runtime.as_ref().map(|over| over.runtime)
    }
}

/// A qualified task identity as `task_output_key` spells it:
/// `source:name` (root scope) or `scope:source#name`.
struct TaskIdentity<'a> {
    scope: &'a str,
    source: TaskSource,
    name: &'a str,
}

impl<'a> TaskIdentity<'a> {
    fn parse(task: &'a str) -> Option<Self> {
        if let Some((prefix, name)) = task.split_once('#') {
            let (scope, source) = prefix
                .rsplit_once(':')
                .map_or(("root", prefix), |(scope, source)| (scope, source));
            return TaskSource::from_label(source).map(|source| Self {
                scope,
                source,
                name,
            });
        }
        let (source, name) = task.split_once(':')?;
        TaskSource::from_label(source).map(|source| Self {
            scope: "root",
            source,
            name,
        })
    }
}

const fn merge_task_verbosity(base: TaskVerbosity, overlay: TaskVerbosity) -> TaskVerbosity {
    TaskVerbosity {
        level: if overlay.level.is_some() {
            overlay.level
        } else {
            base.level
        },
        stream: if overlay.stream.is_some() {
            overlay.stream
        } else {
            base.stream
        },
        stdout: if overlay.stdout.is_some() {
            overlay.stdout
        } else {
            base.stdout
        },
        stderr: if overlay.stderr.is_some() {
            overlay.stderr
        } else {
            base.stderr
        },
        progress: if overlay.progress.is_some() {
            overlay.progress
        } else {
            base.progress
        },
        groups: if overlay.groups.is_some() {
            overlay.groups
        } else {
            base.groups
        },
        task_timing: if overlay.task_timing.is_some() {
            overlay.task_timing
        } else {
            base.task_timing
        },
    }
}

/// What to do when no signal in steps 2–6 matches.
///
/// Set via `--fallback` / `RUNNER_FALLBACK` / `[resolution].fallback`.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FallbackPolicy {
    /// Walk `$PATH` in canonical order and pick the first installed PM.
    /// Errors if nothing matches.
    #[default]
    Probe,
    /// Legacy: silently default to `npm` so dispatch is attempted even
    /// when nothing is detected. Useful for backwards compatibility.
    Npm,
    /// Refuse to proceed when no signal matches; error out with a list of
    /// sources that were checked.
    Error,
}

impl FallbackPolicy {
    /// Every variant, in the order [`Self::label`]'s callers should list
    /// them. Single source of truth for
    /// [`super::policies::parse_fallback_label`] and any surface that
    /// needs to advertise or validate against the same closed set.
    pub(crate) const ALL: [Self; 3] = [Self::Probe, Self::Npm, Self::Error];

    /// The `--fallback` / `RUNNER_FALLBACK` / `[resolution].fallback` label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Npm => "npm",
            Self::Error => "error",
        }
    }
}

/// Install-time lifecycle-script execution policy for `runner install`.
///
/// Lifecycle/build scripts (`postinstall`, native-extension compilation,
/// …) are the primary supply-chain attack surface during dependency
/// installs. This knob lets a project deny them across the package managers
/// that expose a skip mechanism, or force them on across the managers that
/// can express it. The latter matters because several package managers
/// (npm, pnpm, …) are moving to scripts-off-by-default in upcoming majors.
///
/// Set via `--no-scripts` (deny) / `--scripts` (force on) on the CLI,
/// `RUNNER_INSTALL_SCRIPTS` (env), or `[install].scripts` (config), highest
/// first.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ScriptPolicy {
    /// Leave each package manager at its own built-in default: npm,
    /// yarn-classic, pnpm (<10) and composer run dependency scripts, while
    /// bun, pnpm (>=10) and deno already deny them.
    #[default]
    Default,
    /// Skip lifecycle scripts wherever the package manager exposes a skip
    /// mechanism (npm/yarn/pnpm/bun `--ignore-scripts`, composer
    /// `--no-scripts`, yarn-berry `YARN_ENABLE_SCRIPTS=false`); deno already
    /// denies by default. Managers without one (cargo, go, bundler, and the
    /// Python backends uv/poetry/pipenv) warn and proceed unchanged.
    Deny,
    /// Force lifecycle scripts on wherever the package manager can express it:
    /// npm `--no-ignore-scripts`, yarn-berry `YARN_ENABLE_SCRIPTS=true`, deno
    /// `--allow-scripts` (allow all). Managers that already run scripts by
    /// default (composer, cargo, go, bundler, the Python backends, yarn-classic)
    /// are satisfied without a flag. bun and pnpm (>=10) deny dependency build
    /// scripts by default and re-enable them only through a manifest allowlist
    /// (`trustedDependencies` / `onlyBuiltDependencies`) that runner must not
    /// write, so they warn that force-on is not flag-expressible.
    Allow,
}

impl ScriptPolicy {
    /// The two labels a user can actually type. `Default` is the
    /// internal "unset" sentinel, never a valid `[install].scripts` /
    /// `RUNNER_INSTALL_SCRIPTS` value. Single source of truth for
    /// [`super::overrides::parse_script_policy_label`].
    pub(crate) const SETTABLE: [Self; 2] = [Self::Deny, Self::Allow];

    /// The user-facing label, or `None` for [`Self::Default`] (never
    /// user-settable, see [`Self::SETTABLE`]).
    pub(crate) const fn label(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Deny => Some("deny"),
            Self::Allow => Some("allow"),
        }
    }
}

/// How to react when manifest declaration (step 5) and lockfile (step 6)
/// disagree about which package manager the project uses.
///
/// Set via `--on-mismatch` / `RUNNER_ON_MISMATCH` /
/// `[resolution].on_mismatch`. Independent from
/// `devEngines.packageManager` `onFail`. That policy governs whether
/// the *declared* PM can actually run; this one governs whether the
/// resolver tolerates the declaration disagreeing with the install
/// state at all.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum MismatchPolicy {
    /// Emit a `package.json` warning; prefer the declaration (Corepack
    /// semantics, the lockfile is most likely stale).
    #[default]
    Warn,
    /// Stay silent; prefer the declaration.
    Ignore,
    /// Refuse to run and exit non-zero. Intended for CI guardrails where a
    /// mismatch should block the run.
    Error,
}

impl MismatchPolicy {
    /// Every variant, in the order [`Self::label`]'s callers should list
    /// them. Single source of truth for
    /// [`super::policies::parse_mismatch_label`] and any surface that
    /// needs to advertise or validate against the same closed set.
    pub(crate) const ALL: [Self; 3] = [Self::Warn, Self::Ignore, Self::Error];

    /// The `--on-mismatch` / `RUNNER_ON_MISMATCH` / `[resolution].on_mismatch`
    /// label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Ignore => "ignore",
            Self::Error => "error",
        }
    }
}

/// How `runner install` reacts when two or more package managers in the
/// install set write the same directory (a node PM plus a
/// `nodeModulesDir`-enabled Deno both materializing `node_modules/`).
///
/// Set via `[install].on_collision` / `RUNNER_INSTALL_ON_COLLISION`.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CollisionPolicy {
    /// Select one writer per directory. Explicit per-tool install operations
    /// retain every enabled writer and serialize their execution.
    #[default]
    Resolve,
    /// Refuse to install and exit non-zero rather than pick. For CI
    /// guardrails that want an ambiguous tree to block the run.
    Error,
}

impl CollisionPolicy {
    /// Every variant, in the order [`Self::label`]'s callers should list
    /// them. Single source of truth for
    /// [`super::policies::parse_collision_label`] and any surface that
    /// needs to advertise or validate against the same closed set.
    pub(crate) const ALL: [Self; 2] = [Self::Resolve, Self::Error];

    /// The `[install].on_collision` / `RUNNER_INSTALL_ON_COLLISION` label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Error => "error",
        }
    }
}

/// The three environment layers a spawned process sees, narrowest last.
///
/// A value set for a task beats the same value set for the tool running it,
/// which beats the project-wide one, which beats the inherited environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EnvLayers {
    /// `[env]`, every process runner spawns in this project.
    pub project: BTreeMap<String, String>,
    /// `[tools.<name>].env`, keyed by tool label (`mise`, `just`, …).
    pub tool: BTreeMap<String, BTreeMap<String, String>>,
    /// `[tasks.<name>].env`, keyed by task name.
    pub task: BTreeMap<String, BTreeMap<String, String>>,
}

/// A package-manager override plus the source the user set it from.
#[derive(Debug, Clone)]
pub(crate) struct PmOverride {
    /// The chosen package manager.
    pub pm: PackageManager,
    /// Where the override came from.
    pub origin: OverrideOrigin,
}

/// A JS-runtime override plus the source the user set it from.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeOverride {
    /// The chosen runtime.
    pub runtime: JsRuntime,
    /// Where the override came from. Surfaced by `--explain` so the user can
    /// attribute the runtime decision to its origin.
    pub origin: OverrideOrigin,
}

impl RuntimeOverride {
    /// `--explain` trace body, e.g. `bun via --runtime (CLI override)`.
    pub(crate) fn describe(&self) -> String {
        let source = match &self.origin {
            OverrideOrigin::CliFlag => String::from("--runtime (CLI override)"),
            OverrideOrigin::EnvVar => String::from("RUNNER_RUNTIME (environment)"),
            OverrideOrigin::ConfigFile { path } => {
                format!("runner.toml at {}", path.display())
            }
        };
        format!("{} via {source}", self.runtime.label())
    }
}

/// A task-runner override plus the source the user set it from.
#[derive(Debug, Clone)]
pub(crate) struct RunnerOverride {
    /// The chosen task runner.
    pub runner: TaskRunner,
    /// Where the override came from. Surfaced by `--explain` and `doctor`
    /// so the user can attribute the constraint to its origin.
    pub origin: OverrideOrigin,
}

/// Source the user set an override from.
///
/// Listed in precedence order, highest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverrideOrigin {
    /// Set via `--pm` / `--runner` on the command line.
    CliFlag,
    /// Set via `RUNNER_PM` / `RUNNER_RUNNER` in the environment.
    EnvVar,
    /// Set via a `runner.toml` at the project root.
    ConfigFile {
        /// Absolute path the override was loaded from, so `--explain` and
        /// `doctor` can attribute a decision to the exact config file.
        path: PathBuf,
    },
}

impl OverrideOrigin {
    /// Whether an override from this source may be re-exported as an
    /// environment variable to a nested `runner`/`run`. A CLI flag or ambient
    /// env value is invocation-scoped and carries across; a `runner.toml` value
    /// is repo-scoped, and an env layer outranks a nested project's own config,
    /// so re-emitting it would force this repo's setting onto another one.
    pub(crate) const fn propagates_to_nested(&self) -> bool {
        matches!(self, Self::CliFlag | Self::EnvVar)
    }

    /// Render the "via …" provenance fragment for a PM override from
    /// this origin: `via --pm (CLI override)`, `via RUNNER_PM
    /// (environment)`, or `via runner.toml at <path>`.
    pub(crate) fn describe_pm_source(&self) -> String {
        match self {
            Self::CliFlag => "via --pm (CLI override)".to_string(),
            Self::EnvVar => "via RUNNER_PM (environment)".to_string(),
            Self::ConfigFile { path } => format!("via runner.toml at {}", path.display()),
        }
    }
}

/// Sources contributing to a [`ResolutionOverrides`].
///
/// Bundles every CLI/env input the resolver consumes so
/// `ResolutionOverrides::from_sources` stays extensible, adding a new
/// override (say `--on-mismatch` / `RUNNER_ON_MISMATCH`) is one field on
/// this struct, not a positional-argument expansion across every test site.
///
/// Tests typically use `Default` + field updates:
///
/// ```ignore
/// OverrideSources {
///     pm: SourceValue { cli: Some("yarn"), env: None },
///     ..OverrideSources::default()
/// }
/// ```
///
/// Production goes through `ResolutionOverrides::from_cli_and_env`,
/// which builds this from process state.
#[derive(Debug, Default)]
pub(crate) struct OverrideSources<'a> {
    /// `--pm` flag value plus `RUNNER_PM` env.
    pub pm: SourceValue<'a>,
    /// `--runner` flag value plus `RUNNER_RUNNER` env.
    pub runner: SourceValue<'a>,
    /// `--runtime` flag value plus `RUNNER_RUNTIME` env. The `[runtime].js`
    /// config layer is read from `config` rather than carried here.
    pub runtime: SourceValue<'a>,
    /// `--fetch` flag value plus `RUNNER_REACH` env.
    pub reach: SourceValue<'a>,
    /// `--fallback` flag value plus `RUNNER_FALLBACK` env.
    pub fallback: SourceValue<'a>,
    /// `--on-mismatch` flag value plus `RUNNER_ON_MISMATCH` env.
    pub on_mismatch: SourceValue<'a>,
    /// `--no-warnings` flag presence plus `RUNNER_NO_WARNINGS` env.
    pub no_warnings: ExplainSource<'a>,
    /// `-q`/`--quiet` repeat count plus `RUNNER_QUIET` env (numeric or truthy).
    pub quiet: QuietSource<'a>,
    /// `--host-stream` flag value plus `RUNNER_HOST_STREAM` env
    /// (`inherit`|`stderr`).
    pub host_stream: SourceValue<'a>,
    /// `--explain` flag presence plus `RUNNER_EXPLAIN` env.
    pub explain: ExplainSource<'a>,
    /// `-k`/`--keep-going` flag presence plus `RUNNER_KEEP_GOING` env.
    pub keep_going: ExplainSource<'a>,
    /// `--kill-on-fail` flag presence plus `RUNNER_KILL_ON_FAIL` env.
    pub kill_on_fail: ExplainSource<'a>,
    /// `RUNNER_INSTALL_SCRIPTS` env (`deny`|`allow`). The `cli` side stays
    /// unused here; the `--no-scripts`/`--scripts` flags are layered on at the
    /// dispatch boundary; the config side comes from `[install].scripts`.
    pub install_scripts: SourceValue<'a>,
    /// `RUNNER_INSTALL_ON_COLLISION` env (`resolve`|`error`). No CLI flag; the
    /// config side comes from `[install].on_collision`.
    pub install_on_collision: SourceValue<'a>,
    /// Internal `RUNNER_GROUP_ACTIVE` nesting marker a parent runner set on
    /// this process (see `crate::commands::GROUP_ACTIVE_ENV`). Env-only, no CLI or
    /// config side, but captured here so `from_sources` stays a pure function
    /// of its inputs and tests can inject it.
    pub group_active: Option<&'a str>,
    /// Loaded `runner.toml` if any.
    pub config: Option<&'a LoadedConfig>,
}

/// CLI flag plus env-var value for a string-typed override. The
/// resolver trims and de-duplicates these per the precedence chain in
/// `parse_override` (CLI wins; whitespace-only values count as unset).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SourceValue<'a> {
    /// CLI flag value, if the user passed one.
    pub cli: Option<&'a str>,
    /// Env-var value, if set.
    pub env: Option<&'a str>,
}

/// CLI-side resolution overrides (`--pm`, `--runner`, `--runtime`,
/// `--fallback`, `--on-mismatch`) bundled into a single struct, for the same
/// reason [`DiagnosticFlags`] exists: the constructors take one value per
/// axis and would otherwise pass clippy's argument threshold.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CliOverrides<'a> {
    /// `--pm` flag value (env handled inside `from_cli_and_env`).
    pub pm: Option<&'a str>,
    /// `--runner` flag value.
    pub runner: Option<&'a str>,
    /// `--runtime` flag value.
    pub runtime: Option<&'a str>,
    /// `--fetch` flag value.
    pub reach: Option<&'a str>,
    /// `--fallback` flag value.
    pub fallback: Option<&'a str>,
    /// `--on-mismatch` flag value.
    pub on_mismatch: Option<&'a str>,
    /// `--package` flag value.
    pub package: Option<&'a str>,
}

/// CLI-side diagnostic flags (`--no-warnings`, `--quiet`, `--explain`)
/// bundled into a single struct so `ResolutionOverrides::from_cli_and_env`
/// stays under clippy's argument/bool thresholds.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DiagnosticFlags<'a> {
    /// `--no-warnings` flag presence (CLI side only, env handled inside
    /// `from_cli_and_env`).
    pub no_warnings: bool,
    /// `-q`/`--quiet` repeat count (CLI side only, env handled inside
    /// `from_cli_and_env`). `0` = not passed, `1` = `-q`, `2` = `-qq`, `3`+ =
    /// `-qqq`.
    pub quiet: u8,
    /// `--explain` flag presence (CLI side only, env handled inside
    /// `from_cli_and_env`).
    pub explain: bool,
    /// `--host-stream` flag value (CLI side only, env handled inside
    /// `from_cli_and_env`). Bundled here with the other diagnostic flags to
    /// keep the constructors under clippy's argument threshold.
    pub host_stream: Option<&'a str>,
}

/// CLI flag (presence) plus env-var value for a boolean-typed override
/// like `--explain` / `RUNNER_EXPLAIN`. CLI wins; env is interpreted by
/// `super::policies::is_env_truthy`.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ExplainSource<'a> {
    /// `true` when the CLI flag was passed.
    pub cli: bool,
    /// Env-var value, if set.
    pub env: Option<&'a str>,
}

/// CLI repeat count (`-q`/`-qq`/`-qqq`) plus env-var value for the quiet
/// level. CLI > env: a passed count (`cli > 0`) wins outright, else the env
/// applies (`RUNNER_QUIET` is numeric `0..4`, clamped, or a truthy word meaning level 1).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct QuietSource<'a> {
    /// `-q` repeat count from the CLI (`0` when not passed).
    pub cli: u8,
    /// `RUNNER_QUIET` env-var value, if set.
    pub env: Option<&'a str>,
}
