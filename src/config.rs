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
//! node   = "pnpm"      # one of npm|pnpm|yarn|bun|deno
//! python = "uv"        # one of uv|poetry|pipenv
//!
//! [task_runner]
//! prefer = ["just", "turbo"]
//!
//! [resolution]
//! fallback     = "probe"   # probe|npm|error
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
//! Adding a new knob is two changes: a field on the matching section plus a
//! consumer in `crate::resolver`. Keep [`KNOWN_SCHEMA`] in sync so the new
//! key isn't mis-reported as unknown.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::types::{DetectionWarning, Ecosystem, PackageManager};

/// Canonical config filename, written by `runner config init`. Its dotfile form
/// (`.` + this) is the hidden variant; both are accepted during discovery.
pub(crate) const CONFIG_FILENAME: &str = "runner.toml";

/// Directories searched for a config, relative to the loaded directory, highest
/// precedence first: the directory itself (`""`) and its `.config/` subdir.
pub(crate) const CONFIG_DIRS: [&str; 2] = ["", ".config"];

/// Starter `runner.toml` scaffolded by `runner config init`. Generated from
/// [`RunnerConfig`]'s schemars metadata (section/field doc comments) plus a
/// small hand-picked value/hint table, see
/// `cmd::schema::render_init_template`, so a field can't silently ship
/// without scaffold coverage. Regenerate with `just gen-schema` after
/// changing a section struct; a drift-guard test enforces this file stays
/// in sync.
pub(crate) const INIT_TEMPLATE: &str = include_str!("../schemas/runner.init.toml");

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
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct RunnerConfig {
    /// `[pm]`, per-ecosystem package-manager overrides.
    #[serde(default)]
    pub pm: PmSection,
    /// `[tasks]`, persistent task-source preference (global order + per-task pins).
    #[serde(default)]
    pub tasks: TasksSection,
    /// `[task_runner]`, task-runner preferences. Deprecated; superseded
    /// by [`Self::tasks`].
    #[cfg_attr(
        feature = "schema",
        schemars(description = "`[task_runner]`, task-runner preferences. Deprecated; \
                                superseded by `[tasks]`.")
    )]
    #[serde(default, rename = "task_runner")]
    pub task_runner: TaskRunnerSection,
    /// `[install]`, restrict which detected PMs `runner install` runs.
    #[serde(default)]
    pub install: InstallSection,
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
}

/// `[runtime]` section, which JS runtime a task's process tree runs on.
///
/// Separate from `[pm]`: the package manager decides who installs and who
/// invokes the script, the runtime decides what the script and the binaries
/// it shells out to execute on. Overridden by `--runtime` / `RUNNER_RUNTIME`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct RuntimeSection {
    /// JavaScript runtime: `node`, `bun`, or `deno`. Absent leaves the
    /// runtime to the detected package manager, the behaviour before this
    /// key existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub js: Option<String>,
}

/// `[install]` section, restrict which detected package managers
/// `runner install` runs with. Absent or empty installs every detected
/// PM (the default). Overridden by `RUNNER_INSTALL_PMS`.
///
/// Unlike `[pm]` (which scopes *script dispatch* per ecosystem), this
/// scopes the *install fan-out*: in a polyglot repo where both `bun` and
/// `deno` would write `node_modules`, `pms = ["bun"]` keeps install to bun.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct InstallSection {
    /// Allowlist of package-manager labels to install with, e.g.
    /// `["bun"]`. Each must be a detected PM or `runner install` errors.
    /// Empty = install with every detected PM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pms: Vec<String>,

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
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["deny", "allow", null]))
    )]
    pub scripts: Option<String>,

    /// What to do when two or more package managers in the install set write
    /// the same directory (a node PM plus a `nodeModulesDir`-enabled Deno both
    /// materializing `node_modules/`). `"resolve"` (the default) installs with
    /// one writer per directory and shadows the rest, the way a duplicate task
    /// name resolves to one source; listing several writers in `pms` is consent
    /// and runs them all, serialized over the shared tree. `"error"` refuses to
    /// pick and fails instead. Overridden by `RUNNER_INSTALL_ON_COLLISION`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["resolve", "error", null]))
    )]
    pub on_collision: Option<String>,
}

/// `[chain]` section, failure policy for `run -s/-p` chains and
/// `runner install <tasks>`.
// Fields are `Option<bool>` rather than `bool` so the resolver can
// distinguish "user explicitly set false" from "user didn't say":
// env-overrides-config layering means `[chain].keep_going = false` plus
// `RUNNER_KEEP_GOING=1` resolves to `true`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
#[cfg_attr(
    feature = "schema",
    schemars(extend("not" = {
        "required": ["keep_going", "kill_on_fail"],
        "properties": {
            "keep_going": { "const": true },
            "kill_on_fail": { "const": true }
        }
    }))
)]
pub(crate) struct ChainSection {
    /// Run every task in the chain to completion regardless of failures.
    /// Mutually exclusive with `kill_on_fail`. Equivalent to `-k` /
    /// `RUNNER_KEEP_GOING`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_going: Option<bool>,

    /// Parallel only: terminate sibling tasks immediately on first
    /// failure (forcible kill, not graceful shutdown, uncatchable on
    /// Unix). Mutually exclusive with `keep_going`. Equivalent to
    /// `--kill-on-fail` / `RUNNER_KILL_ON_FAIL`. Ignored in sequential
    /// contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kill_on_fail: Option<bool>,
}

/// `[github]` section, GitHub Actions integration. Both knobs only take
/// effect under GitHub Actions (gated at the call site by
/// `actions_rs::env::is_github_actions`); in a normal terminal nothing here
/// changes behavior.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct GitHubSection {
    /// Wrap task output in `runner: <task>` groups under GitHub Actions, and
    /// annotate each failed chain task in the Annotations panel. Defaults to
    /// `true`; set `false` to restore the old undecorated output, including
    /// the live `[task]`-prefixed muxer for parallel runs. `--quiet`
    /// suppresses both independently, since workflow commands are written to
    /// stdout and would otherwise reach a caller parsing it.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Wrap task output in `runner: <task>` groups under GitHub Actions, and \
                           annotate each failed chain task in the Annotations panel. Defaults to \
                           `true`; set `false` to restore the old undecorated output, including \
                           the live `[task]`-prefixed muxer for parallel runs. `--quiet` \
                           suppresses both independently."
        )
    )]
    #[serde(default = "default_group_output")]
    pub group_output: bool,

    /// Under GitHub Actions, group parallel (`-p`) output: buffer each task
    /// and print it as one block on completion instead of interleaving lines
    /// live. Defaults to `true` (CI logs read better grouped), but only when
    /// [`Self::group_output`] is also true. The non-CI equivalent is
    /// `[parallel].grouped` (default `false`), so CI and local diverge unless
    /// you set them to match.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Under GitHub Actions, group parallel (`-p`) output: buffer each task \
                           and print it as one block on completion instead of interleaving lines \
                           live. Defaults to `true`, but only when `group_output` is also true. \
                           The non-CI equivalent is `[parallel].grouped` (default `false`)."
        )
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
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct ParallelSection {
    /// Buffer each parallel task's output and print it as one contiguous
    /// block the moment that task finishes (completion order, first done,
    /// first shown), instead of interleaving prefixed lines live. Defaults to
    /// `false` (the live `[task]`-prefixed muxer); set `true` to group even in
    /// a plain terminal, where a colored header delimits each block.
    #[serde(default)]
    pub grouped: bool,
}

/// `[pm]` section, per-ecosystem package manager overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct PmSection {
    /// Package manager used to dispatch Node `package.json` scripts.
    /// Valid values: `npm`, `pnpm`, `yarn`, `bun`, `deno`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["npm", "pnpm", "yarn", "bun", "deno", null]))
    )]
    pub node: Option<String>,
    /// Package manager used for Python ecosystems.
    /// Valid values: `uv`, `poetry`, `pipenv`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["uv", "poetry", "pipenv", null]))
    )]
    pub python: Option<String>,
}

/// `[task_runner]` section, **deprecated**. Use `[tasks]` instead.
///
/// Kept for backward compatibility: existing `[task_runner].prefer` files
/// keep working (and emit a deprecation warning), but `[tasks].prefer` is the
/// supported successor, rank-only and able to name package managers, not just
/// task runners.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields, extend("deprecated" = true))
)]
pub(crate) struct TaskRunnerSection {
    /// **Deprecated, use `[tasks].prefer` instead** (rank-only, and accepts
    /// package managers like `bun`, not just task runners). Migration:
    /// `[task_runner].prefer = ["turbo"]` → `[tasks].prefer = ["turbo"]`.
    ///
    /// Legacy behavior, still honored: a ranked preference list that
    /// *restricts* candidates to runners in the list (in listed order); a
    /// same-named task under a runner not in the list is hard-rejected.
    /// Valid values: `turbo`, `nx`, `make`, `just`, `task`, `mise`, `bacon`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "schema", schemars(extend("deprecated" = true)))]
    pub prefer: Vec<String>,
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
// No `schemars(deny_unknown_fields)`: the flattened `tasks` map makes this an
// open object (task-name keys become `additionalProperties`), which is
// mutually exclusive with denying unknown fields.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub(crate) struct TasksSection {
    /// Global tie-break order for ambiguous task names, highest priority
    /// first. Listed sources win over unlisted ones (which still run as
    /// lower-priority fallbacks). E.g. `prefer = ["turbo", "bun"]` makes a
    /// `turbo` task win, then a `package.json` script, then everything else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<String>,
    /// **Legacy** per-task source pins that override [`Self::prefer`] for
    /// specific names: `overrides = { dev = "bun", build = "turbo" }`. Superseded
    /// by a task entry's `runner` field (`[tasks.build] runner = "turbo"`), which
    /// carries the same meaning; both are honored and merged (a task entry wins
    /// on conflict). A pin to a source the task doesn't have falls through to the
    /// normal ranking (no hard error).
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Legacy per-task pins that override `prefer` for specific names: \
                           `overrides = { dev = \"bun\", build = \"turbo\" }`. Superseded by a \
                           task entry's `runner` field. A pin to a source the task doesn't have \
                           falls through to the normal ranking (no hard error)."
        )
    )]
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, String>,
    /// Per-task settings keyed by task name (the Cargo-`[dependencies]`-style
    /// map). Reserved keys `prefer`/`overrides` are captured by the fields above;
    /// every other key under `[tasks]` is a task entry.
    #[serde(flatten)]
    pub tasks: BTreeMap<String, TaskSpec>,
}

impl TasksSection {
    /// True when this `[tasks]` section carries a signal that supersedes the
    /// deprecated, *restrictive* `[task_runner].prefer` list: a global `prefer`
    /// rank, a legacy `overrides` pin, or a task entry that contributes a
    /// **source pin** (a bare-string [`TaskSpec::Pin`] or a table with a
    /// `runner` field).
    ///
    /// A verbosity-only task entry (`[tasks.build] verbosity = "quiet"`) names
    /// no source, so it deliberately does *not* count — otherwise adding a
    /// per-task verbosity knob would silently drop a user's legacy runner
    /// restriction (they resolve on entirely separate axes). Judged on the raw
    /// fields, not the parsed result: a recognized-but-source-less label like
    /// `"nx"` still counts as a pin here, matching `parse_tasks_overrides`.
    pub(crate) fn supersedes_legacy_prefer(&self) -> bool {
        !self.prefer.is_empty()
            || !self.overrides.is_empty()
            || self.tasks.values().any(|spec| match spec {
                TaskSpec::Pin(label) => !label.trim().is_empty(),
                TaskSpec::Settings(settings) => settings
                    .runner
                    .as_deref()
                    .is_some_and(|runner| !runner.trim().is_empty()),
            })
    }
}

/// A single `[tasks]` entry, addressed by task name the way a crate is addressed
/// under Cargo's `[dependencies]`: either a bare **string** (shorthand for the
/// task's source/runner pin, e.g. `build = "turbo"`) or a **table** of per-task
/// settings (`build = { runner = "turbo", verbosity = "quiet" }`, or a
/// `[tasks.build]` sub-table).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub(crate) enum TaskSpec {
    /// Shorthand: `build = "turbo"` pins the task's source/runner. Equivalent to
    /// `{ runner = "turbo" }`.
    Pin(String),
    /// Full form: a table of per-task settings.
    Settings(TaskSettings),
}

/// The table form of a [`TaskSpec`]: individual per-task settings, each merged
/// over the built-in defaults so a partial table only overrides what it names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct TaskSettings {
    /// Source/runner pin for this task, same meaning as a legacy
    /// [`TasksSection::overrides`] entry (a runner, package manager, or source
    /// label). A pin the task doesn't have falls through to the normal ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Per-task verbosity, deep-merged over the built-in default and layered
    /// under env/CLI. String shorthand (`"quiet"`) or a `{ level, stream }`
    /// table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<VerbosityConfig>,
}

/// Verbosity intent as written in config: a bare level name (`verbosity =
/// "quiet"`) or a `{ level, stream }` table. String-or-table, the same
/// Cargo-`[dependencies]` shape as [`TaskSpec`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub(crate) enum VerbosityConfig {
    /// `verbosity = "quiet"` — sets the level, leaves stream at its default.
    Level(String),
    /// `verbosity = { level = "quiet", stream = "stderr" }`.
    Table(VerbosityTable),
}

/// The table form of [`VerbosityConfig`]: the two orthogonal knobs, each
/// optional so a partial table deep-merges over the inherited default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct VerbosityTable {
    /// How much of the host's own logging to suppress:
    /// `off` | `quiet` | `very-quiet` | `silent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["off", "quiet", "very-quiet", "silent", null]))
    )]
    pub level: Option<String>,
    /// Whether to keep the host's stdout clean by diverting its diagnostics to
    /// stderr: `inherit` | `stderr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["inherit", "stderr", null]))
    )]
    pub stream: Option<String>,
}

/// `[resolution]` section, resolver policy knobs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct ResolutionSection {
    /// `probe` (default), PATH probe in canonical order when no signals
    /// match; `npm`, legacy silent fallback; `error`, refuse to proceed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["probe", "npm", "error", null]))
    )]
    pub fallback: Option<String>,
    /// `warn` (default), `error`, `ignore`, how to react when declaration
    /// (manifest field) disagrees with detection (lockfile).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["warn", "error", "ignore", null]))
    )]
    pub on_mismatch: Option<String>,
}

/// Recognized sections and their fields, mirroring the section structs and
/// [`INIT_TEMPLATE`]. A key absent from this table is reported as an
/// [`DetectionWarning::UnknownConfigKey`] rather than aborting the load, so a
/// config written by a newer `runner` never bricks an older binary (and vice
/// versa). Keep in sync when adding a section or field; the
/// `known_schema_covers_every_section` test guards section-level drift.
const KNOWN_SCHEMA: &[(&str, &[&str])] = &[
    ("pm", &["node", "python"]),
    ("task_runner", &["prefer"]),
    ("tasks", &["prefer", "overrides"]),
    ("install", &["pms", "scripts", "on_collision"]),
    ("resolution", &["fallback", "on_mismatch"]),
    ("chain", &["keep_going", "kill_on_fail"]),
    ("github", &["group_output", "group_parallel"]),
    ("parallel", &["grouped"]),
    ("runtime", &["js"]),
];

/// Reserved keys under `[tasks]` that are section fields, not task entries.
/// Every other key is a task name; a table-valued task entry has its fields
/// checked against [`TASK_ENTRY_FIELDS`] (see [`collect_unknown_keys`]).
const TASKS_RESERVED_KEYS: &[&str] = &["prefer", "overrides"];

/// Recognized fields of a `[tasks.<name>]` table entry ([`TaskSettings`]).
/// Mirrors the struct; the `known_task_entry_fields_match_schema` test guards
/// drift. An unrecognized field warns (forward-compat) rather than aborting.
const TASK_ENTRY_FIELDS: &[&str] = &["runner", "verbosity"];

/// Recognized fields of a `[tasks.<name>].verbosity` table ([`VerbosityTable`]).
const VERBOSITY_TABLE_FIELDS: &[&str] = &["level", "stream"];

/// Collect forward-compat warnings for sections/fields this build doesn't
/// recognize. Walks the raw parsed table against [`KNOWN_SCHEMA`]; a
/// non-table where a section is expected is left for the typed deserialize to
/// reject (a genuine type error, not version skew).
pub(crate) fn collect_unknown_keys(value: &toml::Value) -> Vec<DetectionWarning> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for (section, body) in table {
        let Some((_, known_fields)) = KNOWN_SCHEMA.iter().find(|(name, _)| name == section) else {
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

/// Field-level forward-compat check for the `[tasks]` open map. A task entry is
/// either a string shorthand (a source pin — no fields to check) or a table
/// whose fields must be in [`TASK_ENTRY_FIELDS`], with its `verbosity` sub-table
/// (when a table) checked against [`VERBOSITY_TABLE_FIELDS`]. Unknown fields are
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
            if !TASK_ENTRY_FIELDS.contains(&field.as_str()) {
                warnings.push(DetectionWarning::UnknownConfigKey {
                    path: format!("tasks.{name}.{field}"),
                });
                continue;
            }
            if field == "verbosity"
                && let Some(verbosity) = value.as_table()
            {
                for sub in verbosity.keys() {
                    if !VERBOSITY_TABLE_FIELDS.contains(&sub.as_str()) {
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
    let mut warnings = collect_unknown_keys(&value);
    let config: RunnerConfig = value
        .try_into()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    warnings.extend(deprecation_warnings(&config));

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

/// Migration warnings for config keys that still work but have a supported
/// successor. Shared by [`load`] and the editor language server so both surface
/// the same nudge.
///
/// `[task_runner].prefer` is superseded by `[tasks]`: the warning flags whether
/// `[tasks]` overrides it this run so the message tells the truth either way.
pub(crate) fn deprecation_warnings(config: &RunnerConfig) -> Vec<DetectionWarning> {
    let mut out = Vec::new();
    if !config.task_runner.prefer.is_empty() {
        let prefer_set = !config.tasks.prefer.is_empty();
        let overrides_set = !config.tasks.overrides.is_empty();
        // Name whichever `[tasks]` knob actually superseded this run, so the
        // message never claims `tasks.prefer` is set when only `overrides` is.
        let replacement = if overrides_set && !prefer_set {
            "tasks.overrides"
        } else {
            "tasks.prefer"
        };
        out.push(DetectionWarning::DeprecatedConfigKey {
            path: "task_runner.prefer".to_string(),
            replacement,
            // Share the resolver's exact supersession predicate so the warning
            // can never disagree with what actually happened: a task entry with
            // a source pin supersedes too, while a verbosity-only entry does not.
            superseded: config.tasks.supersedes_legacy_prefer(),
        });
    }
    out
}

/// Validate `[pm].node` against the set of script-dispatching PMs.
///
/// # Errors
///
/// Returns an error if `raw` does not name a known PM, or if it names a PM
/// that cannot run `package.json` scripts (e.g. `cargo`).
pub(crate) fn parse_node_pm(raw: &str) -> Result<PackageManager> {
    let pm = PackageManager::from_label(raw)
        .ok_or_else(|| anyhow!("[pm].node: unknown package manager {raw:?}"))?;
    let eco = pm.ecosystem();
    if !matches!(eco, Ecosystem::Node | Ecosystem::Deno) {
        return Err(anyhow!(
            "[pm].node: {} cannot dispatch package.json scripts (it belongs to ecosystem {:?})",
            pm.label(),
            eco,
        ));
    }
    Ok(pm)
}

/// Validate `[pm].python` against the Python ecosystem.
///
/// # Errors
///
/// Returns an error if `raw` does not name a known PM or if the named PM
/// is not part of the Python ecosystem.
pub(crate) fn parse_python_pm(raw: &str) -> Result<PackageManager> {
    let pm = PackageManager::from_label(raw)
        .ok_or_else(|| anyhow!("[pm].python: unknown package manager {raw:?}"))?;
    if pm.ecosystem() != Ecosystem::Python {
        return Err(anyhow!(
            "[pm].python: {} is not a Python package manager",
            pm.label(),
        ));
    }
    Ok(pm)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        CONFIG_FILENAME, INIT_TEMPLATE, KNOWN_SCHEMA, LoadedConfig, RunnerConfig, load,
        parse_node_pm, parse_python_pm,
    };
    use crate::tool::test_support::TempDir;
    use crate::types::{DetectionWarning, PackageManager};

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
        assert_eq!(loaded.config.pm.node.as_deref(), Some("npm"));
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
        assert_eq!(loaded.config.pm.node.as_deref(), Some("npm"));
    }

    #[test]
    fn legacy_task_runner_prefer_warns_deprecated() {
        let dir = TempDir::new("config-deprecated-task-runner");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[task_runner]\nprefer = [\"turbo\"]\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                DetectionWarning::DeprecatedConfigKey {
                    superseded: false,
                    ..
                }
            )),
            "expected a non-superseded deprecation warning, got: {:?}",
            loaded.warnings,
        );
    }

    #[test]
    fn tasks_section_marks_legacy_prefer_superseded() {
        let dir = TempDir::new("config-deprecated-superseded");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks]\nprefer = [\"bun\"]\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                DetectionWarning::DeprecatedConfigKey {
                    superseded: true,
                    ..
                }
            )),
            "expected a superseded deprecation warning, got: {:?}",
            loaded.warnings,
        );
    }

    #[test]
    fn tasks_overrides_alone_names_itself_as_the_replacement() {
        let dir = TempDir::new("config-deprecated-overrides-only");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks.overrides]\nbuild = \"bun\"\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                DetectionWarning::DeprecatedConfigKey {
                    superseded: true,
                    replacement: "tasks.overrides",
                    ..
                }
            )),
            "expected the warning to name tasks.overrides, got: {:?}",
            loaded.warnings,
        );
    }

    #[test]
    fn verbosity_only_task_entry_does_not_supersede_legacy_prefer() {
        // Regression (F1): a verbosity-only `[tasks.<name>]` entry names no
        // source, so it must NOT flip the legacy `[task_runner].prefer`
        // restriction off — otherwise adding a per-task verbosity knob silently
        // changes task-source resolution.
        let dir = TempDir::new("config-verbosity-only-not-superseding");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks.build]\nverbosity = \"quiet\"\n",
        )
        .expect("seed config");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(
            !loaded.config.tasks.supersedes_legacy_prefer(),
            "a verbosity-only task entry must not supersede [task_runner].prefer",
        );
        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                DetectionWarning::DeprecatedConfigKey {
                    superseded: false,
                    ..
                }
            )),
            "deprecation warning must report superseded: false, got: {:?}",
            loaded.warnings,
        );
    }

    #[test]
    fn task_pin_entry_supersedes_legacy_prefer() {
        // A real source pin (bare string or a `runner` field) still supersedes,
        // preserving the intended behavior of the open `[tasks]` map.
        for body in [
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks]\nbuild = \"turbo\"\n",
            "[task_runner]\nprefer = [\"turbo\"]\n\n[tasks.build]\nrunner = \"turbo\"\n",
        ] {
            let dir = TempDir::new("config-pin-supersedes");
            fs::write(dir.path().join(CONFIG_FILENAME), body).expect("seed config");
            let loaded = load(dir.path())
                .expect("config should parse")
                .expect("config should be present");
            assert!(
                loaded.config.tasks.supersedes_legacy_prefer(),
                "a task entry with a source pin should supersede; body: {body}",
            );
            assert!(
                loaded.warnings.iter().any(|w| matches!(
                    w,
                    DetectionWarning::DeprecatedConfigKey {
                        superseded: true,
                        ..
                    }
                )),
                "expected superseded: true for a pin entry; body: {body}, got: {:?}",
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

    #[cfg(feature = "schema")]
    #[test]
    fn known_task_entry_fields_match_schema() {
        // Drift guard: the const field lists `collect_unknown_task_keys` checks
        // against must match the real structs, or a renamed/added field would
        // be spuriously warned about (or a typo silently tolerated).
        use std::collections::BTreeSet;

        fn schema_props<T: schemars::JsonSchema>() -> BTreeSet<String> {
            let schema =
                serde_json::to_value(schemars::schema_for!(T)).expect("schema should serialize");
            schema["properties"]
                .as_object()
                .expect("struct schema must have properties")
                .keys()
                .cloned()
                .collect()
        }

        assert_eq!(
            super::TASK_ENTRY_FIELDS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<BTreeSet<_>>(),
            schema_props::<super::TaskSettings>(),
            "TASK_ENTRY_FIELDS must match TaskSettings",
        );
        assert_eq!(
            super::VERBOSITY_TABLE_FIELDS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<BTreeSet<_>>(),
            schema_props::<super::VerbosityTable>(),
            "VERBOSITY_TABLE_FIELDS must match VerbosityTable",
        );
        // `TasksSection`'s named (non-flattened) fields ARE the reserved keys;
        // the flattened `tasks` map is `additionalProperties`, not a property,
        // so it never appears here. If a reserved field is added/renamed without
        // updating TASKS_RESERVED_KEYS, `collect_unknown_task_keys` would treat
        // it as a task entry (or vice versa) — this guard catches that.
        assert_eq!(
            super::TASKS_RESERVED_KEYS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<BTreeSet<_>>(),
            schema_props::<super::TasksSection>(),
            "TASKS_RESERVED_KEYS must match TasksSection's named fields",
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
        assert_eq!(loaded.config.pm.node.as_deref(), Some("pnpm"));
        assert_eq!(loaded.config.pm.python.as_deref(), Some("uv"));
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
        assert_eq!(loaded.config.pm.node.as_deref(), Some("bun"));
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
    fn known_schema_matches_init_template_sections_and_fields() {
        // Guard KNOWN_SCHEMA against drift in both directions, at section AND
        // field granularity. The scaffold ships every non-deprecated knob
        // (commented out), so its sections/fields are the canonical set
        // modulo deprecated sections (see DEPRECATED_SECTIONS below), which
        // `render_init_template` deliberately omits so new users never get
        // handed one; a field missing from KNOWN_SCHEMA makes `config init`
        // write a file that warns about its own keys, while a stale
        // KNOWN_SCHEMA entry lists a field nobody can set. Equality catches
        // either, so adding a struct field forces the template and
        // KNOWN_SCHEMA to be updated alongside it.
        use std::collections::{BTreeMap, BTreeSet};

        // Sections KNOWN_SCHEMA recognizes (for backward-compat parsing) but
        // that `render_init_template` intentionally leaves out of the
        // scaffold because they're deprecated.
        const DEPRECATED_SECTIONS: &[&str] = &["task_runner"];

        // Walk the template into section -> {field names it emits}.
        let mut template: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut section: Option<String> = None;
        for line in INIT_TEMPLATE.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix('[') {
                section = Some(rest.trim_end_matches(']').to_string());
                template
                    .entry(section.clone().expect("just set"))
                    .or_default();
                continue;
            }
            // Field lines are `key = ...`, shipped commented-out. Strip one
            // leading `#`, then keep only a bare-identifier left of `=`; that
            // shape excludes the prose comments, which carry no `key =`.
            let body = trimmed.strip_prefix('#').map_or(trimmed, str::trim);
            let Some((lhs, _)) = body.split_once('=') else {
                continue;
            };
            let key = lhs.trim();
            if !key.is_empty()
                && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && let Some(sec) = &section
            {
                template
                    .get_mut(sec)
                    .expect("section recorded above")
                    .insert(key.to_string());
            }
        }

        let mut known: BTreeMap<String, BTreeSet<String>> = KNOWN_SCHEMA
            .iter()
            .map(|(name, fields)| {
                (
                    (*name).to_string(),
                    fields.iter().map(|f| (*f).to_string()).collect(),
                )
            })
            .collect();
        for section in DEPRECATED_SECTIONS {
            known.remove(*section);
        }

        assert_eq!(
            template, known,
            "INIT_TEMPLATE sections/fields must match KNOWN_SCHEMA (minus DEPRECATED_SECTIONS) \
             exactly, keep the section structs, the scaffold template, and KNOWN_SCHEMA in sync \
             when adding a knob"
        );
    }

    #[cfg(feature = "schema")]
    #[test]
    fn known_schema_matches_generated_runner_config_schema() {
        // known_schema_matches_init_template_sections_and_fields only
        // catches INIT_TEMPLATE drifting from KNOWN_SCHEMA, a struct field
        // added to a section without updating either the scaffold or
        // KNOWN_SCHEMA passes that guard invisibly (template and KNOWN_SCHEMA
        // still agree with each other, just not with the real type; the
        // typed deserializer would accept the field while
        // `collect_unknown_keys` spuriously flags it as unknown). Compare
        // KNOWN_SCHEMA directly against the schemars-derived shape of
        // RunnerConfig, independent of the scaffold.
        use std::collections::{BTreeMap, BTreeSet};

        let schema = serde_json::to_value(schemars::schema_for!(RunnerConfig))
            .expect("RunnerConfig schema should serialize");

        let top_properties = schema["properties"]
            .as_object()
            .expect("RunnerConfig schema must have top-level properties");
        let defs = schema["$defs"]
            .as_object()
            .expect("RunnerConfig schema must have $defs");

        let generated: BTreeMap<String, BTreeSet<String>> = top_properties
            .iter()
            .map(|(section, section_schema)| {
                let def_name = section_schema["$ref"]
                    .as_str()
                    .and_then(|r| r.strip_prefix("#/$defs/"))
                    .unwrap_or_else(|| {
                        panic!(
                            "{section}: expected a $defs $ref in the generated schema, got \
                             {section_schema:?}"
                        )
                    });
                let fields = defs[def_name]["properties"]
                    .as_object()
                    .unwrap_or_else(|| {
                        panic!("{def_name}: expected a properties object in the generated schema")
                    })
                    .keys()
                    .cloned()
                    .collect();
                (section.clone(), fields)
            })
            .collect();

        let known: BTreeMap<String, BTreeSet<String>> = KNOWN_SCHEMA
            .iter()
            .map(|(name, fields)| {
                (
                    (*name).to_string(),
                    fields.iter().map(|f| (*f).to_string()).collect(),
                )
            })
            .collect();

        assert_eq!(
            generated, known,
            "KNOWN_SCHEMA must match RunnerConfig's real (schemars-derived) shape exactly, a \
             struct field with no KNOWN_SCHEMA entry is silently treated as unknown by \
             collect_unknown_keys even though the typed deserializer accepts it"
        );
    }

    #[test]
    fn parse_node_pm_accepts_node_and_deno() {
        assert_eq!(parse_node_pm("pnpm").unwrap(), PackageManager::Pnpm);
        assert_eq!(parse_node_pm("bun").unwrap(), PackageManager::Bun);
        assert_eq!(parse_node_pm("deno").unwrap(), PackageManager::Deno);
    }

    #[test]
    fn parse_node_pm_rejects_cross_ecosystem() {
        let err = parse_node_pm("cargo").expect_err("cargo should not be a Node PM");
        assert!(format!("{err}").contains("cannot dispatch package.json scripts"));
    }

    #[test]
    fn parse_python_pm_accepts_uv_poetry_pipenv() {
        assert_eq!(parse_python_pm("uv").unwrap(), PackageManager::Uv);
        assert_eq!(parse_python_pm("poetry").unwrap(), PackageManager::Poetry);
        assert_eq!(parse_python_pm("pipenv").unwrap(), PackageManager::Pipenv);
    }

    #[test]
    fn parse_python_pm_rejects_node_pm() {
        let err = parse_python_pm("pnpm").expect_err("pnpm should not be Python");
        assert!(format!("{err}").contains("not a Python package manager"));
    }

    #[test]
    fn load_parses_install_section() {
        let dir = TempDir::new("config-install");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[install]\npms = [\"bun\", \"cargo\"]\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert_eq!(loaded.config.install.pms, vec!["bun", "cargo"]);
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
