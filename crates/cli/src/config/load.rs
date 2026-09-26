//! `runner.toml`, project-level configuration.
//!
//! The file lives at the project root. The resolver reads it as step 4 of
//! the precedence chain (after CLI flags and environment variables, before
//! manifest declarations).
//!
//! Schema:
//!
//! ```toml
//! [pm]
//! node   = "pnpm"
//! python = "uv"
//!
//! [tasks]
//! prefer = ["just", "turbo"]
//!
//! [resolution]
//! fallback     = "probe"   # probe|error
//! on_mismatch  = "warn"    # warn|error|ignore
//! ```
//!
//! Parsing is **forward-compatible**: an unknown section or field (a typo,
//! or a key a newer `runner` added) is ignored rather than fatal, so a
//! config written by one version never bricks task dispatch under another.
//! Unknown keys are still surfaced as warnings (see [`collect_unknown_keys`])
//! so genuine typos stay visible. The JSON Schema keeps
//! `additionalProperties: false` (via `schemars(deny_unknown_fields)`), so
//! editors flag typos inline even though the runtime tolerates them.
//!
//! Adding a new knob is two changes: a field on the matching section and a
//! consumer in `crate::resolver`. The schema derived from the field is what
//! makes the key recognized and documented.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::provider::Named;
use crate::types::DetectionWarning;
use runner_core::ProviderId;

/// Canonical config filename, written by `runner config init`. Its dotfile form
/// (`.` + this) is the hidden variant; both are accepted during discovery.
pub(crate) const CONFIG_FILENAME: &str = "runner.toml";

/// Directories searched for a config, relative to the loaded directory, highest
/// precedence first: the directory itself (`""`) and its `.config/` subdir.
pub(crate) const CONFIG_DIRS: [&str; 2] = ["", ".config"];

/// Parsed `runner.toml` content plus the absolute path it was loaded from.
#[derive(Debug, Clone)]
pub(crate) struct LoadedConfig {
    /// Absolute path the config was read from. Echoed back in resolver
    /// traces and the `runner doctor` output (Phase 6).
    pub path: PathBuf,
    /// Parsed config sections.
    pub config: RunnerConfig,
    /// Unknown sections/fields the parse tolerated (forward compat). Carried
    /// so the dispatcher can fold them into `ctx.warnings` and `config
    /// validate` can report them, instead of silently dropping them.
    pub warnings: Vec<DetectionWarning>,
}

/// Top-level schema for `runner.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(
    deny_unknown_fields,
    extend("$id" = crate::schema::config_schema_url())
)]
pub(crate) struct RunnerConfig {
    /// `[runner]`, independent runner-authored output categories.
    #[serde(default)]
    pub runner: RunnerOutputSection,
    /// `[host]`, host-tool diagnostic policy.
    #[serde(default)]
    pub host: HostOutputSection,
    /// `[pm]`, per-ecosystem package-manager overrides.
    #[serde(default)]
    #[schemars(description = runner_core::Setting::doc_for("pm"))]
    pub pm: PmSection,
    /// `[tasks]`, persistent task-source preference (global order + per-task pins).
    #[serde(default)]
    pub tasks: TasksSection,
    /// `[install]`, lifecycle-script and shared-directory policy for installs.
    #[serde(default)]
    pub install: InstallSection,
    /// `[defaults]`, a project's standing answer to per-invocation flags.
    #[serde(default)]
    pub defaults: DefaultsSection,
    /// `[resolution]`, resolver-policy knobs.
    #[serde(default)]
    pub resolution: ResolutionSection,
    /// `[chain]`, failure policy for multi-task chains.
    #[serde(default)]
    pub chain: ChainSection,
    /// `[github]`, GitHub Actions integration (output grouping).
    #[serde(default)]
    pub github: GitHubSection,
    /// `[parallel]`, presentation of parallel (`-p`) chain output.
    #[serde(default)]
    pub parallel: ParallelSection,
    /// `[runtime]`, which JS runtime executes tasks and local files.
    #[serde(default)]
    pub runtime: RuntimeSection,
    /// `[env]`, variables set on every process runner spawns in this project.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(description = runner_core::Setting::doc_for("env"))]
    pub env: BTreeMap<String, String>,
    /// `[tools]`, per-tool settings keyed by tool label (`mise`, `just`, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolSettings>,
}

/// `[tools.<name>]`, settings scoped to one detected tool.
///
/// Narrower than `[env]` and wider than a task entry, so a value here reaches
/// every invocation of that tool and nothing else.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ToolSettings {
    /// Which of the tool's operations `runner install` runs, in order.
    ///
    /// `true` is `["install"]`, `false` is `[]`, and a bare string is a
    /// one-element list. Only mise defines more than one operation today
    /// (`install` and `bootstrap`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = runner_core::Setting::doc_for("tools.<name>.install"))]
    pub install: Option<ToolInstall>,
    /// Variables set on every invocation of this tool.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(description = runner_core::Setting::doc_for("tools.<name>.env"))]
    pub env: BTreeMap<String, String>,
}

/// `[tools.<name>].install` as written: a toggle, one operation name, or an
/// ordered list. All three normalize to a list via [`ToolInstall::operations`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum ToolInstall {
    /// `install = true` / `install = false`.
    Toggle(bool),
    /// `install = "bootstrap"`.
    One(String),
    /// `install = ["bootstrap", "install"]`.
    Many(Vec<String>),
}

impl ToolInstall {
    /// The operations to run, in order. `true` means the tool's default
    /// operation, which every tool spells `install`.
    pub(crate) fn operations(&self) -> Vec<String> {
        match self {
            Self::Toggle(true) => vec!["install".to_string()],
            Self::Toggle(false) => Vec::new(),
            Self::One(name) => vec![name.clone()],
            Self::Many(names) => names.clone(),
        }
    }
}

/// `[runner]` output categories. An absent field inherits the selected quiet
/// preset. These settings apply when no explicit `-q`/`RUNNER_QUIET` preset was
/// selected for the invocation.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RunnerOutputSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub progress: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub warnings: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub errors: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub groups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub task_timing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub summary: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub fatal_errors: Option<bool>,
}

impl RunnerOutputSection {
    /// The configured value for `output`, when the key is present.
    pub(crate) const fn get(&self, output: crate::tool::RunnerOutput) -> Option<bool> {
        use crate::tool::RunnerOutput;
        match output {
            RunnerOutput::Progress => self.progress,
            RunnerOutput::Warnings => self.warnings,
            RunnerOutput::Errors => self.errors,
            RunnerOutput::Groups => self.groups,
            RunnerOutput::TaskTiming => self.task_timing,
            RunnerOutput::Summary => self.summary,
            RunnerOutput::FatalErrors => self.fatal_errors,
        }
    }
}

/// `[host]` host-tool output policy. Diagnostics never controls task streams;
/// adapters clamp unsupported requests to their strongest safe mode.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct HostOutputSection {
    /// `normal`, `quiet`, or `reduced`. Unsupported reductions are safely
    /// clamped by each adapter and reported by `--explain`. Absent, each
    /// task's `[tasks.<name>].verbosity` decides; any value here, `normal`
    /// included, overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["normal", "quiet", "reduced", null]))]
    pub diagnostics: Option<String>,
    /// `inherit` or `stderr`. Per-task stream settings override this global
    /// default; CLI/env still outrank both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["inherit", "stderr", null]))]
    #[schemars(extend("default" = crate::tool::Stream::default().label()))]
    pub stream: Option<String>,
}

/// `[runtime]` section, which JS runtime a task's process tree runs on.
///
/// Separate from `[pm]`: the package manager decides who installs and who
/// invokes the script, the runtime decides what the script and the binaries
/// it shells out to execute on. Overridden by `--runtime` / `RUNNER_RUNTIME`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RuntimeSection {
    /// JavaScript runtime label. Absent leaves the runtime to the detected
    /// package manager.
    #[schemars(
        description = runner_core::Setting::doc_for("runtime.js"),
        extend("enum" = js_runtime_labels())
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js: Option<String>,
}

fn js_runtime_labels() -> Vec<Option<&'static str>> {
    crate::provider::js_runtimes()
        .into_iter()
        .map(|runtime| Some(runtime.label()))
        .chain([None])
        .collect()
}

/// Lifecycle-script and shared-directory policy for installation.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct InstallSection {
    /// Lifecycle-script policy for the install. `"deny"` skips lifecycle
    /// scripts wherever the package manager exposes a skip mechanism
    /// (npm/yarn/pnpm/bun `--ignore-scripts`, composer `--no-scripts`,
    /// yarn-berry `YARN_ENABLE_SCRIPTS=false`; deno already denies by
    /// default), warning for the managers that cannot. `"allow"` forces
    /// scripts on wherever a manager can express it (npm `--no-ignore-scripts`,
    /// yarn-berry `YARN_ENABLE_SCRIPTS=true`, deno `--allow-scripts`); managers
    /// that already run scripts by default are satisfied without a flag, while
    /// bun and pnpm (>=10) warn because re-enabling their dependency build
    /// scripts needs a manifest allowlist (`trustedDependencies` /
    /// `onlyBuiltDependencies`) runner won't write. Absent leaves every manager
    /// at its default. Overridden by `RUNNER_INSTALL_SCRIPTS`, then the
    /// `--no-scripts` / `--scripts` flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = runner_core::Setting::doc_for("install.scripts"),
        extend("enum" = nullable(runner_core::Setting::choices_for("install.scripts")))
    )]
    pub scripts: Option<String>,

    /// What to do when two or more package managers in the install set write
    /// the same directory (a node PM plus a `nodeModulesDir`-enabled Deno both
    /// materializing `node_modules/`). `"resolve"` (the default) installs with
    /// one writer per directory and shadows the rest, the way a duplicate task
    /// name resolves to one source. Explicit per-tool install operations retain
    /// multiple writers and serialize their execution. `"error"` refuses to
    /// pick and fails instead. Overridden by `RUNNER_INSTALL_ON_COLLISION`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["resolve", "error", null]))]
    #[schemars(extend("default" = crate::resolver::CollisionPolicy::default().label()))]
    pub on_collision: Option<String>,
}

/// `[chain]` section, failure policy for `run -s/-p` chains and
/// `runner install <tasks>`.
// Fields are `Option<bool>` rather than `bool` so the resolver can
// distinguish "user explicitly set false" from "user didn't say":
// env-overrides-config layering means `[chain].keep_going = false` plus
// `RUNNER_KEEP_GOING=1` resolves to `true`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
#[schemars(extend("not" = {
    "required": ["keep_going", "kill_on_fail"],
    "properties": {
        "keep_going": { "const": true },
        "kill_on_fail": { "const": true }
    }
}))]
pub(crate) struct ChainSection {
    /// Run every task in the chain to completion regardless of failures.
    /// Mutually exclusive with `kill_on_fail`. Equivalent to `-k` /
    /// `RUNNER_KEEP_GOING`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = false))]
    pub keep_going: Option<bool>,

    /// Parallel only: terminate sibling tasks immediately on first
    /// failure (forcible kill, not graceful shutdown, uncatchable on
    /// Unix). Mutually exclusive with `keep_going`. Equivalent to
    /// `--kill-on-fail` / `RUNNER_KILL_ON_FAIL`. Ignored in sequential
    /// contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = false))]
    pub kill_on_fail: Option<bool>,
}

/// `[github]` section, GitHub Actions integration. Both knobs only take
/// effect under GitHub Actions (gated at the call site by
/// `actions_rs::env::is_github_actions`); in a normal terminal nothing here
/// changes behavior.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct GitHubSection {
    /// Wrap task output in `runner: <task>` groups under GitHub Actions, and
    /// annotate each failed chain task in the Annotations panel. Defaults to
    /// `true`; set `false` to restore the old undecorated output, including
    /// the live `[task]`-prefixed muxer for parallel runs. `--quiet`
    /// suppresses both independently, since workflow commands are written to
    /// stdout and would otherwise reach a caller parsing it.
    #[schemars(
        description = "Wrap task output in `runner: <task>` groups under GitHub Actions, and \
                       annotate each failed chain task in the Annotations panel. Defaults to \
                       `true`; set `false` to restore the old undecorated output, including the \
                       live `[task]`-prefixed muxer for parallel runs. `--quiet` suppresses both \
                       independently."
    )]
    #[serde(default = "default_group_output")]
    pub group_output: bool,

    /// Under GitHub Actions, group parallel (`-p`) output: buffer each task
    /// and print it as one block on completion instead of interleaving lines
    /// live. Defaults to `true` (CI logs read better grouped), but only when
    /// [`Self::group_output`] is also true. The non-CI equivalent is
    /// `[parallel].grouped` (default `false`), so CI and local diverge unless
    /// you set them to match.
    #[schemars(
        description = "Under GitHub Actions, group parallel (`-p`) output: buffer each task and \
                       print it as one block on completion instead of interleaving lines live. \
                       Defaults to `true`, but only when `group_output` is also true. The non-CI \
                       equivalent is `[parallel].grouped` (default `false`)."
    )]
    #[serde(default = "default_github_group_parallel")]
    pub group_parallel: bool,
}

impl Default for GitHubSection {
    fn default() -> Self {
        Self {
            group_output: default_group_output(),
            group_parallel: default_github_group_parallel(),
        }
    }
}

/// Default for [`GitHubSection::group_output`]: grouping is on unless the
/// user opts out, so the CI-readability win is automatic.
const fn default_group_output() -> bool {
    true
}

/// Default for [`GitHubSection::group_parallel`]: under GitHub Actions,
/// parallel output is grouped by default for readable CI logs.
const fn default_github_group_parallel() -> bool {
    true
}

/// `[parallel]` section, how parallel (`-p`) chains present their output
/// **outside** GitHub Actions. (Under GitHub Actions, see
/// `[github].group_parallel` instead.)
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ParallelSection {
    /// Buffer each parallel task's output and print it as one contiguous
    /// block the moment that task finishes (completion order, first done,
    /// first shown), instead of interleaving prefixed lines live. Defaults to
    /// `false` (the live `[task]`-prefixed muxer); set `true` to group even in
    /// a plain terminal, where a colored header delimits each block.
    #[serde(default)]
    pub grouped: bool,
}

/// `[pm]` section, the package manager per ecosystem label.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub(crate) struct PmSection(pub BTreeMap<String, String>);

impl schemars::JsonSchema for PmSection {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PmSection".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut properties = serde_json::Map::new();
        for source in crate::provider::managed_sources() {
            let mut labels: Vec<serde_json::Value> = crate::provider::dispatchers(source)
                .into_iter()
                .map(|pm| pm.label().into())
                .collect();
            labels.push(serde_json::Value::Null);
            properties.insert(
                source.ecosystem().label().to_owned(),
                serde_json::json!({
                    "description": format!(
                        "Package manager that dispatches {} tasks.",
                        source.label()
                    ),
                    "enum": labels,
                }),
            );
        }
        schemars::json_schema!({
            "description": "`[pm]` section, the package manager per ecosystem.",
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        })
    }
}

/// `[tasks]` section, per-task configuration keyed by task name, plus the two
/// reserved cross-task knobs `prefer` and `overrides`.
///
/// A task entry works like a crate under Cargo's `[dependencies]`: the key is
/// the task name and the value is either a **string** shorthand for the task's
/// source/runner pin (`build = "turbo"`) or a **table** of per-task settings
/// (`build = { runner = "turbo", verbosity = "quiet" }`, or a `[tasks.build]`
/// sub-table). `prefer` (global rank) and `overrides` (legacy per-task pin map,
/// superseded by a task entry's `runner`) are reserved names, so a task literally
/// called `prefer` or `overrides` cannot use the map form.
///
/// The pin vocabulary is shared: a label is a task runner (`turbo`, `make`, …),
/// a package manager (`bun`, `npm`, `pnpm`, `yarn`, `deno`, …), or a source
/// name (`package.json`, `deno`, …). Package-manager labels map to the script
/// source they run (`bun` → `package.json`). Selection is **rank-only**: it
/// never hard-rejects an unlisted source, only reorders. An explicit CLI
/// qualifier (`package.json:test`), `--runner`, or `--pm`/`RUNNER_PM` still
/// outranks these file settings.
///
/// Keys layer from least to most specific: `site`, `package.json:site`,
/// `rfc:site` (workspace member `rfc`), `rfc:package.json#site` (the FQN
/// `doctor --json` prints).
// No `schemars(deny_unknown_fields)`: the flattened `tasks` map makes this an
// open object (task-name keys become `additionalProperties`), which is
// mutually exclusive with denying unknown fields.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub(crate) struct TasksSection {
    /// Global tie-break order for ambiguous task names, highest priority
    /// first. Listed sources win over unlisted ones (which still run as
    /// lower-priority fallbacks). E.g. `prefer = ["turbo", "bun"]` makes a
    /// `turbo` task win, then a `package.json` script, then everything else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(
        description = runner_core::Setting::doc_for("tasks.prefer"),
        with = "Vec<TaskPin>"
    )]
    pub prefer: Vec<String>,
    /// **Legacy** per-task source pins that override [`Self::prefer`] for
    /// specific names: `overrides = { dev = "bun", build = "turbo" }`. Superseded
    /// by a task entry's `runner` field (`[tasks.build] runner = "turbo"`), which
    /// carries the same meaning; both are honored and merged (a task entry wins
    /// on conflict). A pin to a source the task doesn't have falls through to the
    /// normal ranking (no hard error).
    #[schemars(
        description = "Legacy per-task pins that override `prefer` for specific names: `overrides \
                       = { dev = \"bun\", build = \"turbo\" }`. Superseded by a task entry's \
                       `runner` field. A pin to a source the task doesn't have falls through to \
                       the normal ranking (no hard error).",
        with = "BTreeMap<String, TaskPin>"
    )]
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, String>,
    /// Per-task settings keyed by task name (the Cargo-`[dependencies]`-style
    /// map). Reserved keys `prefer`/`overrides` are captured by the fields above;
    /// every other key under `[tasks]` is a task entry.
    #[serde(flatten)]
    pub tasks: BTreeMap<String, TaskSpec>,
}

/// A single `[tasks]` entry, addressed by task name the way a crate is addressed
/// under Cargo's `[dependencies]`: either a bare **string** (shorthand for the
/// task's source/runner pin, e.g. `build = "turbo"`) or a **table** of per-task
/// settings (`build = { runner = "turbo", verbosity = "quiet" }`, or a
/// `[tasks.build]` sub-table).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum TaskSpec {
    /// Shorthand: `build = "turbo"` pins the task's source/runner. Equivalent to
    /// `{ runner = "turbo" }`.
    #[schemars(with = "TaskPin")]
    Pin(String),
    /// Full form: a table of per-task settings.
    Settings(TaskSettings),
}

/// The table form of a [`TaskSpec`]: individual per-task settings, each merged
/// over the built-in defaults so a partial table only overrides what it names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct TaskSettings {
    /// Source/runner pin for this task, same meaning as a legacy
    /// [`TasksSection::overrides`] entry (a runner, package manager, or source
    /// label). A pin the task doesn't have falls through to the normal ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<TaskPin>")]
    pub runner: Option<String>,
    /// Per-task verbosity, deep-merged over the built-in default and layered
    /// under env/CLI. String shorthand (`"quiet"`) or a `{ level, stream }`
    /// table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<VerbosityConfig>,
    /// Preserve or discard this task's stdout independently of quiet presets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["inherit", "discard", null]))]
    pub stdout: Option<String>,
    /// Preserve or discard this task's stderr independently of quiet presets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["inherit", "discard", null]))]
    pub stderr: Option<String>,
    /// Print this task's dispatch arrow. `false` hides it for this task only;
    /// a quiet preset or `[runner].progress = false` hides it regardless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<bool>,
    /// Wrap this task's output in a GitHub Actions group. `false` opts this
    /// task out; `[runner].groups = false` or a quiet preset wins over `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<bool>,
    /// Print this task's chain timing line. `false` hides it for this task
    /// only; `[runner].task_timing = false` or a quiet preset wins over `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_timing: Option<bool>,
    /// Variables set on this task's process, over the tool and project layers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(description = runner_core::Setting::doc_for("tasks.<name>.env"))]
    pub env: BTreeMap<String, String>,
}

/// Verbosity intent as written in config: a bare level name (`verbosity =
/// "quiet"`) or a `{ level, stream }` table. String-or-table, the same
/// Cargo-`[dependencies]` shape as [`TaskSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum VerbosityConfig {
    /// `verbosity = "quiet"` — sets the level, leaves stream at its default.
    #[schemars(extend("enum" = ["off", "quiet", "very-quiet", "silent"]))]
    Level(String),
    /// `verbosity = { level = "quiet", stream = "stderr" }`.
    Table(VerbosityTable),
}

/// The table form of [`VerbosityConfig`]: the two orthogonal knobs, each
/// optional so a partial table deep-merges over the inherited default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct VerbosityTable {
    /// How much of the host's own logging to suppress:
    /// `off` | `quiet` | `very-quiet` | `silent` | `mute`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["off", "quiet", "very-quiet", "silent", "mute", null]))]
    pub level: Option<String>,
    /// Whether to keep the host's stdout clean by diverting its diagnostics to
    /// stderr: `inherit` | `stderr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["inherit", "stderr", null]))]
    pub stream: Option<String>,
}

/// `[resolution]` section, resolver policy knobs.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ResolutionSection {
    /// `probe` (default) takes a package manager from PATH for a task source
    /// with no package manager evidence; `error` refuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["probe", "error", null]))]
    #[schemars(extend("default" = crate::resolver::FallbackPolicy::default().label()))]
    pub fallback: Option<String>,
    /// `warn` (default), `error`, `ignore`, how to react when declaration
    /// (manifest field) disagrees with detection (lockfile).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = ["warn", "error", "ignore", null]))]
    #[schemars(extend("default" = crate::resolver::MismatchPolicy::default().label()))]
    pub on_mismatch: Option<String>,
}

/// `[defaults]` section, the value a flag takes when the invocation omits it.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct DefaultsSection {
    /// Whether a command that can download may run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = runner_core::Setting::doc_for("defaults.fetch"),
        extend("enum" = nullable(runner_core::Setting::choices_for("defaults.fetch")))
    )]
    pub fetch: Option<String>,
    /// Install without touching the lockfile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = runner_core::Setting::doc_for("defaults.frozen"))]
    pub frozen: Option<bool>,
}

/// A `[tasks]` pin: a task runner, package manager, or source label.
struct TaskPin;

impl schemars::JsonSchema for TaskPin {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TaskPin".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": crate::types::task_source_labels(),
        })
    }
}

/// `choices` plus `null`, the values an optional closed-choice key accepts.
fn nullable(choices: &[&'static str]) -> Vec<Option<&'static str>> {
    choices.iter().copied().map(Some).chain([None]).collect()
}

/// `RunnerConfig`'s schema, generated once per process.
pub(crate) fn schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(RunnerConfig))
            .expect("RunnerConfig schema serializes")
    })
}

/// The field names a `$defs` entry declares, in declaration order.
pub(crate) fn def_fields(def: &str) -> Vec<&'static str> {
    schema()["$defs"][def]["properties"]
        .as_object()
        .map(|props| props.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// Top-level section name to the `$defs` entry describing it.
pub(crate) fn section_def(section: &str) -> Option<&'static str> {
    schema()["properties"][section]["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/$defs/"))
}

/// Sections whose keys the user chooses rather than runner: `[env]` holds
/// variable names, `[tools]` holds tool labels.
pub(crate) const OPEN_MAP_SECTIONS: &[&str] = &["env", "tools"];

/// The fields recognized under `section`, from the schema. A key absent from
/// it is reported as a [`DetectionWarning::UnknownConfigKey`] rather than
/// aborting the load, so a config written by a newer `runner` never bricks an
/// older binary, and vice versa.
fn known_fields(section: &str) -> Option<Vec<&'static str>> {
    section_def(section).map(def_fields)
}

/// Reserved keys under `[tasks]` that are section fields, not task entries.
/// Every other key is a task name; a table-valued task entry has its fields
/// checked against `TaskSettings` (see [`collect_unknown_keys`]).
const TASKS_RESERVED_KEYS: &[&str] = &["prefer", "overrides"];

fn tool_entry_fields() -> Vec<&'static str> {
    def_fields("ToolSettings")
}

fn task_entry_fields() -> Vec<&'static str> {
    def_fields("TaskSettings")
}

fn verbosity_table_fields() -> Vec<&'static str> {
    def_fields("VerbosityTable")
}

/// Collect forward-compat warnings for sections/fields this build doesn't
/// recognize. Walks the raw parsed table against the schema; a
/// non-table where a section is expected is left for the typed deserialize to
/// reject (a genuine type error, not version skew).
pub(crate) fn collect_unknown_keys(value: &toml::Value) -> Vec<DetectionWarning> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for (section, body) in table {
        if OPEN_MAP_SECTIONS.contains(&section.as_str()) {
            // `[env]` keys are variable names; `[tools]` keys are tool
            // labels. Neither can be an unknown *field*, but a `[tools]`
            // entry's own fields are fixed, so recurse one level there.
            if section == "tools"
                && let Some(body) = body.as_table()
            {
                collect_unknown_tool_keys(body, &mut warnings);
            }
            continue;
        }
        let Some(known_fields) = known_fields(section) else {
            warnings.push(DetectionWarning::UnknownConfigKey {
                path: section.clone(),
            });
            continue;
        };
        if let Some(body) = body.as_table() {
            // `[tasks]` is an open map (task name → settings) with only
            // `prefer`/`overrides` reserved, so a top-level key that isn't one
            // of those is a task *name* (arbitrary — never an "unknown field").
            // But a task's own *settings* have a fixed field set, so recurse one
            // level to catch a typo like `[tasks.build] runer = "turbo"`, which
            // would otherwise be silently dropped. Warnings keep forward-compat
            // (a newer runner's field is tolerated, not fatal).
            if section == "tasks" {
                collect_unknown_task_keys(body, &mut warnings);
                continue;
            }
            for field in body.keys() {
                if !known_fields.contains(&field.as_str()) {
                    warnings.push(DetectionWarning::UnknownConfigKey {
                        path: format!("{section}.{field}"),
                    });
                }
            }
        }
    }
    warnings
}

/// Field-level forward-compat check for the `[tools]` open map. Tool labels
/// are arbitrary, so only an entry's own fields are checked, against
/// `ToolSettings`. `env` under an entry is itself an open map and is
/// not recursed into.
fn collect_unknown_tool_keys(tools: &toml::value::Table, warnings: &mut Vec<DetectionWarning>) {
    for (name, entry) in tools {
        let Some(fields) = entry.as_table() else {
            continue;
        };
        for field in fields.keys() {
            if !tool_entry_fields().contains(&field.as_str()) {
                warnings.push(DetectionWarning::UnknownConfigKey {
                    path: format!("tools.{name}.{field}"),
                });
            }
        }
    }
}

/// Field-level forward-compat check for the `[tasks]` open map. A task entry is
/// either a string shorthand (a source pin — no fields to check) or a table
/// whose fields must be in `TaskSettings`, with its `verbosity` sub-table
/// (when a table) checked against `VerbosityTable`. Unknown fields are
/// warned about (dotted path `tasks.<name>.<field>` /
/// `tasks.<name>.verbosity.<sub>`), not errors, so a config from a newer runner
/// still loads. Reserved section keys (`prefer`/`overrides`) are skipped.
fn collect_unknown_task_keys(tasks: &toml::value::Table, warnings: &mut Vec<DetectionWarning>) {
    for (name, entry) in tasks {
        if TASKS_RESERVED_KEYS.contains(&name.as_str()) {
            continue;
        }
        let Some(fields) = entry.as_table() else {
            // A string-shorthand pin (`build = "turbo"`) has no fields.
            continue;
        };
        for (field, value) in fields {
            if !task_entry_fields().contains(&field.as_str()) {
                warnings.push(DetectionWarning::UnknownConfigKey {
                    path: format!("tasks.{name}.{field}"),
                });
                continue;
            }
            if field == "verbosity"
                && let Some(verbosity) = value.as_table()
            {
                for sub in verbosity.keys() {
                    if !verbosity_table_fields().contains(&sub.as_str()) {
                        warnings.push(DetectionWarning::UnknownConfigKey {
                            path: format!("tasks.{name}.verbosity.{sub}"),
                        });
                    }
                }
            }
        }
    }
}

/// Load the project config, searching [`CONFIG_DIRS`] × plain/dotted
/// [`CONFIG_FILENAME`] in precedence order.
///
/// Returns `Ok(None)` when no candidate exists; `Ok(Some(_))` otherwise, with
/// `LoadedConfig::path` set to the file actually loaded. The parse is
/// forward-compatible: unknown sections/fields are tolerated (and returned as
/// `warnings`) so version skew never aborts the load. Genuine failures,
/// unreadable file, malformed TOML, or a wrong-typed *known* field, still
/// propagate as errors.
///
/// # Errors
///
/// Returns an error if a candidate file exists but cannot be read, isn't valid
/// TOML, or assigns the wrong type to a recognized field.
pub(crate) fn load(dir: &Path) -> Result<Option<LoadedConfig>> {
    let Some((path, content)) = read_first_candidate(dir)? else {
        return Ok(None);
    };

    // Parse once into a generic value: it lets us surface unknown keys as
    // warnings (forward compat) while still letting a wrong-typed known field
    // fail the typed conversion below.
    let value: toml::Value =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    let warnings = collect_unknown_keys(&value);
    let config: RunnerConfig = value
        .try_into()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(Some(LoadedConfig {
        path,
        config,
        warnings,
    }))
}

/// Read the first config file that exists, searching each [`CONFIG_DIRS`]
/// directory for the plain then dotted [`CONFIG_FILENAME`]. Directory precedence
/// is outer, so a config in the directory itself beats one in its `.config/`.
/// Returns the path and contents; `Ok(None)` when none exist.
///
/// # Errors
///
/// Propagates any read error other than "not found" (e.g. a permission error),
/// so a present-but-unreadable config never masquerades as absent.
fn read_first_candidate(dir: &Path) -> Result<Option<(PathBuf, String)>> {
    let dotted = format!(".{CONFIG_FILENAME}");
    let filenames = [CONFIG_FILENAME, dotted.as_str()];
    for subdir in CONFIG_DIRS {
        let base = if subdir.is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(subdir)
        };
        for filename in filenames {
            let path = base.join(filename);
            match fs::read_to_string(&path) {
                Ok(content) => return Ok(Some((path, content))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("failed to read {}", path.display()));
                }
            }
        }
    }
    Ok(None)
}

/// Validate `[pm].<ecosystem>`: `raw` must name a package manager that
/// dispatches the task source of `ecosystem`. `Ok(None)` for an ecosystem
/// without such a source, which the load already warned about.
///
/// # Errors
///
/// Returns an error if `raw` names no package manager, or one that cannot
/// dispatch the ecosystem's task source.
pub(crate) fn parse_pm(
    ecosystem: &str,
    raw: &str,
) -> Result<Option<(runner_core::Ecosystem, ProviderId)>> {
    let Some(source) = crate::provider::managed_sources()
        .into_iter()
        .find(|source| source.ecosystem().label() == ecosystem)
    else {
        return Ok(None);
    };
    let pm = crate::provider::package_manager(raw)
        .ok_or_else(|| anyhow!("[pm].{ecosystem}: unknown package manager {raw:?}"))?;
    if !crate::provider::dispatchers(source).contains(&pm) {
        return Err(anyhow!(
            "[pm].{ecosystem}: {} cannot dispatch {} tasks",
            pm.label(),
            source.label(),
        ));
    }
    Ok(Some((source.ecosystem(), pm)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{CONFIG_FILENAME, LoadedConfig, RunnerConfig, load, parse_pm};
    use crate::tool::test_support::TempDir;
    use crate::types::DetectionWarning;
    use runner_core::ProviderId;

    /// Dotted paths of the unknown-key warnings a load produced.
    fn unknown_paths(loaded: &LoadedConfig) -> Vec<String> {
        loaded
            .warnings
            .iter()
            .filter_map(|w| match w {
                DetectionWarning::UnknownConfigKey { path } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = TempDir::new("config-absent");
        let result = load(dir.path()).expect("absent file should be Ok(None)");

        assert!(result.is_none());
    }

    #[test]
    fn load_discovers_hidden_dotfile() {
        let dir = TempDir::new("config-hidden");
        fs::write(dir.path().join(".runner.toml"), "[pm]\nnode = \"npm\"\n")
            .expect("seed hidden config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect(".runner.toml should be discovered");

        assert!(loaded.path.ends_with(".runner.toml"));
    }

    #[test]
    fn load_discovers_config_dir_variant() {
        let dir = TempDir::new("config-dot-config-dir");
        fs::create_dir_all(dir.path().join(".config")).expect("mk .config");
        fs::write(
            dir.path().join(".config/runner.toml"),
            "[pm]\nnode = \"npm\"\n",
        )
        .expect("seed .config config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect(".config/runner.toml should be discovered");

        assert!(loaded.path.ends_with("runner.toml"));
        assert!(loaded.path.to_string_lossy().contains(".config"));
    }

    #[test]
    fn load_prefers_canonical_over_fallbacks() {
        let dir = TempDir::new("config-precedence");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n")
            .expect("seed canonical");
        fs::write(dir.path().join(".runner.toml"), "[pm]\nnode = \"bun\"\n").expect("seed hidden");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.path.ends_with(CONFIG_FILENAME));
        assert_eq!(
            loaded.config.pm.0.get("node").map(String::as_str),
            Some("npm")
        );
    }

    #[test]
    fn load_prefers_root_over_config_dir() {
        let dir = TempDir::new("config-dir-precedence");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n")
            .expect("seed root config");
        fs::create_dir_all(dir.path().join(".config")).expect("mk .config");
        fs::write(
            dir.path().join(".config/runner.toml"),
            "[pm]\nnode = \"bun\"\n",
        )
        .expect("seed .config config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(!loaded.path.to_string_lossy().contains(".config"));
        assert_eq!(
            loaded.config.pm.0.get("node").map(String::as_str),
            Some("npm")
        );
    }

    #[test]
    fn every_declared_config_key_carries_its_declaration() {
        let schema = super::schema();
        for setting in runner_core::SETTINGS {
            let node = setting_path(schema, setting.key)
                .and_then(|path| schema.pointer(&format!("/{}", path.join("/"))));
            let Some(node) = node.filter(|_| setting.config) else {
                assert!(
                    node.is_none() && !setting.config,
                    "{}: config={} disagrees with the schema",
                    setting.key,
                    setting.config
                );
                continue;
            };
            assert_eq!(node["description"], setting.doc, "{}", setting.key);
            if let runner_core::SettingKind::Choice(choices) = setting.kind {
                assert_eq!(
                    node["enum"],
                    serde_json::json!(super::nullable(choices)),
                    "{}",
                    setting.key
                );
            }
        }
    }

    /// The JSON pointer segments of a dotted settings key in `schema`.
    fn setting_path(schema: &serde_json::Value, key: &str) -> Option<Vec<String>> {
        let parts: Vec<&str> = key.split('.').collect();
        find_path(schema, schema, &parts, Vec::new())
    }

    fn find_path(
        schema: &serde_json::Value,
        node: &serde_json::Value,
        parts: &[&str],
        path: Vec<String>,
    ) -> Option<Vec<String>> {
        let Some((part, rest)) = parts.split_first() else {
            return Some(path);
        };
        if let Some(reference) = node
            .get("$ref")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.strip_prefix("#/"))
        {
            let target = schema.pointer(&format!("/{reference}"))?;
            return find_path(
                schema,
                target,
                parts,
                reference.split('/').map(str::to_owned).collect(),
            );
        }
        for (branch, alternatives) in ["anyOf", "oneOf"]
            .iter()
            .filter_map(|branch| Some((branch, node.get(branch)?.as_array()?)))
        {
            for (index, alternative) in alternatives.iter().enumerate() {
                let mut nested = path.clone();
                nested.extend([(*branch).to_owned(), index.to_string()]);
                if let Some(found) = find_path(schema, alternative, parts, nested) {
                    return Some(found);
                }
            }
        }
        let segments = if part.starts_with('<') {
            vec!["additionalProperties"]
        } else {
            vec!["properties", part]
        };
        let mut path = path;
        let mut node = node;
        for segment in segments {
            node = node.get(segment)?;
            path.push(segment.to_owned());
        }
        find_path(schema, node, rest, path)
    }

    #[test]
    fn defaults_supply_fetch_and_frozen() {
        let dir = TempDir::new("config-defaults");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[defaults]\nfetch = \"local\"\nfrozen = true\n",
        )
        .expect("seed config");
        let loaded = load(dir.path()).unwrap().unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.config.defaults.fetch.as_deref(), Some("local"));
        assert_eq!(loaded.config.defaults.frozen, Some(true));
    }

    #[test]
    fn a_task_runner_section_is_an_unknown_key_whatever_tasks_says() {
        for body in [
            "[task_runner]\nprefer = [\"turbo\"]\n",
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks]\nprefer = [\"bun\"]\n",
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks.overrides]\nbuild = \"bun\"\n",
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks.build]\nverbosity = \"quiet\"\n",
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks]\nbuild = \"turbo\"\n",
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks.build]\nrunner = \"turbo\"\n",
        ] {
            let dir = TempDir::new("config-task-runner-unknown");
            fs::write(dir.path().join(CONFIG_FILENAME), body).expect("seed config");
            let loaded = load(dir.path())
                .expect("config should parse")
                .expect("config should be present");
            assert_eq!(
                unknown_paths(&loaded),
                vec!["task_runner".to_string()],
                "body: {body}"
            );
            assert!(
                loaded
                    .warnings
                    .iter()
                    .all(|w| matches!(w, DetectionWarning::UnknownConfigKey { .. })),
                "body: {body}, got: {:?}",
                loaded.warnings,
            );
        }
    }

    #[test]
    fn unknown_task_entry_field_warns_instead_of_silently_dropping() {
        // Regression (F2): a typo'd per-task field (`runer` for `runner`) used
        // to parse to an empty settings entry with the pin silently lost. It
        // must now surface as an unknown-key warning (forward-compat: a warning,
        // not a hard error).
        let dir = TempDir::new("config-task-field-typo");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[tasks]\nbuild = { runer = \"turbo\" }\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should still load (forward-compat)")
            .expect("config should be present");
        assert_eq!(unknown_paths(&loaded), ["tasks.build.runer"]);
    }

    #[test]
    fn unknown_verbosity_subfield_warns() {
        let dir = TempDir::new("config-verbosity-subfield-typo");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[tasks.build]\nverbosity = { levl = \"quiet\" }\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should load")
            .expect("config should be present");
        assert!(
            unknown_paths(&loaded).contains(&"tasks.build.verbosity.levl".to_string()),
            "expected a tasks.build.verbosity.levl warning, got: {:?}",
            unknown_paths(&loaded),
        );
    }

    #[test]
    fn valid_task_entries_produce_no_unknown_key_warnings() {
        // No false positives: reserved keys, string shorthand, and the two real
        // per-task fields (incl. a full verbosity table) are all recognized.
        let dir = TempDir::new("config-task-entries-clean");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[tasks]\nprefer = [\"turbo\"]\noverrides = { dev = \"bun\" }\nbuild = \
             \"turbo\"\n\n[tasks.test]\nrunner = \"bun\"\nverbosity = { level = \"quiet\", stream \
             = \"stderr\" }\n\n[tasks.lint]\nverbosity = \"silent\"\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");
        assert!(
            unknown_paths(&loaded).is_empty(),
            "valid task entries must not warn, got: {:?}",
            unknown_paths(&loaded),
        );
    }

    #[test]
    fn tasks_section_validates() {
        // `[tasks]` with a PM label and a per-task pin is a valid config,
        // the same check `runner config validate` runs.
        let dir = TempDir::new("config-tasks-valid");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[tasks]\nprefer = [\"turbo\", \"bun\"]\n\n[tasks.overrides]\nbuild = \"turbo\"\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");
        crate::resolver::validate_config(&loaded).expect("a well-formed [tasks] section validates");
    }

    #[test]
    fn load_parses_pm_section() {
        let dir = TempDir::new("config-pm");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[pm]\nnode = \"pnpm\"\npython = \"uv\"\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.path.ends_with(CONFIG_FILENAME));
        assert_eq!(
            loaded.config.pm.0.get("node").map(String::as_str),
            Some("pnpm")
        );
        assert_eq!(
            loaded.config.pm.0.get("python").map(String::as_str),
            Some("uv")
        );
    }

    #[test]
    fn load_warns_on_unknown_section_without_failing() {
        // Forward compat: a section this build doesn't know (a typo, or one a
        // newer runner added) must not abort the load; it warns and the rest
        // of the config still applies.
        let dir = TempDir::new("config-unknown-key");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[pm]\nnode = \"bun\"\n[zoot]\nfoo = 1\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown section must be tolerated, not fatal")
            .expect("config should be present");

        assert_eq!(unknown_paths(&loaded), vec!["zoot".to_string()]);
        // Known config beside the unknown section is still honored.
        assert_eq!(
            loaded.config.pm.0.get("node").map(String::as_str),
            Some("bun")
        );
    }

    #[test]
    fn load_warns_on_unknown_field_within_known_section() {
        let dir = TempDir::new("config-unknown-pm-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nrust = \"cargo\"\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown field must be tolerated, not fatal")
            .expect("config should be present");

        assert_eq!(unknown_paths(&loaded), vec!["pm.rust".to_string()]);
    }

    #[test]
    fn load_still_rejects_wrong_type_on_known_field() {
        // Forward compat tolerates *unknown* keys, not garbage in *known*
        // ones: a wrong-typed known field is a genuine error, still fatal.
        let dir = TempDir::new("config-wrong-type");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[github]\ngroup_output = \"yes\"\n",
        )
        .expect("config should be written");

        let err = load(dir.path()).expect_err("wrong type on a known field must stay fatal");
        assert!(format!("{err:#}").contains("failed to parse"));
    }

    #[test]
    fn tool_install_normalizes_every_spelling() {
        use super::ToolInstall;
        assert_eq!(ToolInstall::Toggle(true).operations(), ["install"]);
        assert_eq!(ToolInstall::Toggle(false).operations().len(), 0);
        assert_eq!(
            ToolInstall::One("bootstrap".into()).operations(),
            ["bootstrap"]
        );
        assert_eq!(
            ToolInstall::Many(vec!["bootstrap".into(), "install".into()]).operations(),
            ["bootstrap", "install"],
        );
    }

    #[test]
    fn tool_install_parses_all_four_toml_forms() {
        for (written, expected) in [
            ("install = true", vec!["install"]),
            ("install = false", vec![]),
            (r#"install = "bootstrap""#, vec!["bootstrap"]),
            (
                r#"install = ["bootstrap", "install"]"#,
                vec!["bootstrap", "install"],
            ),
        ] {
            let config: RunnerConfig =
                toml::from_str(&format!("[tools.mise]\n{written}\n")).expect("parses");
            let install = config.tools["mise"]
                .install
                .as_ref()
                .expect("install is set");
            assert_eq!(install.operations(), expected, "{written}");
        }
    }

    #[test]
    fn open_map_sections_do_not_warn_about_their_keys() {
        let doc: toml::Value = toml::from_str(
            "[env]\nANYTHING = \"1\"\n\n[tools.mise]\ninstall = true\nenv = { A = \"b\" }\n",
        )
        .expect("parses");
        assert_eq!(super::collect_unknown_keys(&doc).len(), 0);
    }

    #[test]
    fn a_tool_entry_field_typo_still_warns() {
        let doc: toml::Value = toml::from_str("[tools.mise]\ninstalll = true\n").expect("parses");
        let paths: Vec<String> = super::collect_unknown_keys(&doc)
            .into_iter()
            .map(|w| match w {
                DetectionWarning::UnknownConfigKey { path } => path,
                other => panic!("unexpected warning: {other:?}"),
            })
            .collect();
        assert_eq!(paths, ["tools.mise.installl"]);
    }

    #[test]
    fn parse_pm_accepts_every_dispatcher_of_the_ecosystem_source() {
        use runner_core::Ecosystem;

        assert_eq!(
            parse_pm("node", "pnpm").unwrap(),
            Some((Ecosystem::Node, ProviderId::Pnpm))
        );
        assert_eq!(
            parse_pm("node", "deno").unwrap(),
            Some((Ecosystem::Node, ProviderId::Deno))
        );
        assert_eq!(
            parse_pm("python", "pipenv").unwrap(),
            Some((Ecosystem::Python, ProviderId::Pipenv))
        );
    }

    #[test]
    fn parse_pm_rejects_a_package_manager_of_another_source() {
        let err = parse_pm("node", "cargo").expect_err("cargo cannot dispatch package.json");
        assert!(format!("{err}").contains("cannot dispatch package.json tasks"));
        let err = parse_pm("python", "pnpm").expect_err("pnpm cannot dispatch pyproject");
        assert!(format!("{err}").contains("cannot dispatch pyproject.toml tasks"));
    }

    #[test]
    fn parse_pm_skips_an_ecosystem_without_a_managed_source() {
        assert_eq!(parse_pm("rust", "cargo").unwrap(), None);
    }

    #[test]
    fn install_has_no_allowlist_only_per_tool_vetoes() {
        let dir = TempDir::new("config-install");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[install]\npms = [\"bun\", \"cargo\"]\n\n[tools.cargo]\ninstall = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert_eq!(unknown_paths(&loaded), vec!["install.pms".to_string()]);
        let veto = loaded.config.tools["cargo"]
            .install
            .as_ref()
            .expect("the veto is the surviving install knob");
        assert_eq!(veto.operations().len(), 0);
    }

    #[test]
    fn load_parses_install_scripts() {
        let dir = TempDir::new("config-install-scripts");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[install]\nscripts = \"deny\"\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert_eq!(loaded.config.install.scripts.as_deref(), Some("deny"));
    }

    #[test]
    fn load_warns_on_unknown_install_key() {
        let dir = TempDir::new("config-unknown-install-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[install]\nfoo = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [install] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["install.foo".to_string()]);
    }

    #[test]
    fn load_parses_chain_section() {
        let dir = TempDir::new("config-chain");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[chain]\nkeep_going = true\nkill_on_fail = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert_eq!(loaded.config.chain.keep_going, Some(true));
        assert_eq!(loaded.config.chain.kill_on_fail, Some(false));
    }

    #[test]
    fn load_warns_on_unknown_chain_key() {
        let dir = TempDir::new("config-unknown-chain-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[chain]\nfast = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [chain] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["chain.fast".to_string()]);
    }

    #[test]
    fn load_parses_github_section() {
        let dir = TempDir::new("config-github");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[github]\ngroup_output = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(!loaded.config.github.group_output);
    }

    #[test]
    fn github_group_output_defaults_true_when_key_omitted() {
        let dir = TempDir::new("config-github-default");
        fs::write(dir.path().join(CONFIG_FILENAME), "[github]\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.github.group_output);
    }

    #[test]
    fn github_group_output_defaults_true_when_section_absent() {
        let dir = TempDir::new("config-github-absent");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.github.group_output);
    }

    #[test]
    fn load_warns_on_unknown_github_key() {
        let dir = TempDir::new("config-unknown-github-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[github]\nfoo = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [github] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["github.foo".to_string()]);
    }

    #[test]
    fn load_parses_parallel_grouped() {
        let dir = TempDir::new("config-parallel-grouped");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[parallel]\ngrouped = true\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.parallel.grouped);
    }

    #[test]
    fn parallel_grouped_defaults_false_when_section_absent() {
        let dir = TempDir::new("config-parallel-default");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        // Off by default outside GitHub Actions.
        assert!(!loaded.config.parallel.grouped);
    }

    #[test]
    fn load_warns_on_unknown_parallel_key() {
        let dir = TempDir::new("config-unknown-parallel-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[parallel]\nfoo = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [parallel] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["parallel.foo".to_string()]);
    }

    #[test]
    fn load_parses_github_group_parallel() {
        let dir = TempDir::new("config-github-group-parallel");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[github]\ngroup_parallel = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(!loaded.config.github.group_parallel);
        // group_output is independent and still defaults true.
        assert!(loaded.config.github.group_output);
    }

    #[test]
    fn github_group_parallel_defaults_true() {
        let dir = TempDir::new("config-github-group-parallel-default");
        fs::write(dir.path().join(CONFIG_FILENAME), "[github]\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.github.group_parallel);
    }
}
