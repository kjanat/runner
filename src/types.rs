//! Shared types used across detection, commands, and tool modules.

use std::path::PathBuf;

/// A language/runtime ecosystem that owns one or more package managers.
///
/// Used by the resolver to scope overrides — a `[pm].node = "pnpm"` entry
/// in `runner.toml` applies only when resolving for [`Ecosystem::Node`].
/// Deno is its own ecosystem even though its package manager can also
/// dispatch `package.json` scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Ecosystem {
    /// Node.js (npm, yarn, pnpm, bun).
    Node,
    /// Deno.
    Deno,
    /// Python (uv, poetry, pipenv).
    Python,
    /// Rust (cargo).
    Rust,
    /// Go.
    Go,
    /// Ruby (bundler).
    Ruby,
    /// PHP (composer).
    Php,
}

impl Ecosystem {
    /// Lower-case label used in human messages, JSON output, and
    /// override origins. Single source of truth so `doctor --json` and
    /// resolver warnings agree on the spelling.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Deno => "deno",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Ruby => "ruby",
            Self::Php => "php",
        }
    }
}

/// A dependency manager detected via lockfile or config presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PackageManager {
    /// npm — detected via `package-lock.json`.
    Npm,
    /// Yarn — detected via `yarn.lock`.
    Yarn,
    /// pnpm — detected via `pnpm-lock.yaml`.
    Pnpm,
    /// Bun — detected via `bun.lockb` or `bun.lock`.
    Bun,
    /// Cargo (Rust) — detected via `Cargo.toml`.
    Cargo,
    /// Deno — detected via `deno.json` / `deno.jsonc`.
    Deno,
    /// uv (Python) — detected via `uv.lock`.
    Uv,
    /// Poetry (Python) — detected via `poetry.lock` or Poetry `pyproject.toml` markers.
    Poetry,
    /// Pipenv (Python) — detected via `Pipfile` / `Pipfile.lock`.
    Pipenv,
    /// Go modules — detected via `go.mod`.
    Go,
    /// Bundler (Ruby) — detected via `Gemfile`.
    Bundler,
    /// Composer (PHP) — detected via `composer.json`.
    Composer,
}

/// A task runner detected via config file presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TaskRunner {
    /// Turborepo — detected via `turbo.json` / `turbo.jsonc`.
    Turbo,
    /// Nx — detected via `nx.json`.
    Nx,
    /// GNU Make — detected via `Makefile` / `GNUmakefile` / `makefile`.
    Make,
    /// just — detected via case-insensitive `justfile` / `.justfile`.
    Just,
    /// go-task — detected via `Taskfile.yml` and variants.
    GoTask,
    /// mise — detected via `mise.toml` / `.mise.toml`.
    Mise,
    /// bacon — detected via `bacon.toml`.
    Bacon,
}

/// A runnable task extracted from a project config file.
#[derive(Debug, Clone)]
pub(crate) struct Task {
    /// Name as it appears in the config (e.g. `"dev"`, `"build"`).
    pub name: String,
    /// Which config file this task was extracted from.
    pub source: TaskSource,
    /// Tool-specific execution target. Used by Go packages to keep the
    /// display name separate from the `go run` target (`.` vs `./cmd/name`).
    pub run_target: Option<String>,
    /// Optional human-readable description (e.g. justfile doc comment,
    /// go-task `desc` field).
    pub description: Option<String>,
    /// When this task is an alias, the name of the target recipe it
    /// resolves to (e.g. `alias b := build` → `Some("build")`).
    pub alias_of: Option<String>,
    /// `Some(runner)` when this task's command body is a thin
    /// passthrough to a task runner for a same-named target — e.g. a
    /// `package.json` script `"build": "just build"` records
    /// `Some(TaskRunner::Just)`. Set during detection by inspecting the
    /// actual script body, not inferred from name collisions, so real
    /// scripts like `"build": "vite build"` are never flagged. Used by
    /// completion to avoid emitting a redundant `package.json:build`
    /// candidate alongside the underlying runner's `build` task.
    pub passthrough_to: Option<TaskRunner>,
}

/// Identifies the config file a [`Task`] was extracted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TaskSource {
    /// Node package manifest `"scripts"` field (`package.json`,
    /// `package.json5`, `package.yaml`).
    PackageJson,
    /// Makefile target.
    Makefile,
    /// justfile recipe.
    Justfile,
    /// go-task `Taskfile.yml` task.
    Taskfile,
    /// `turbo.json` / `turbo.jsonc` `"tasks"` (v2) or `"pipeline"` (v1).
    TurboJson,
    /// `deno.json` / `deno.jsonc` `"tasks"` field.
    DenoJson,
    /// Cargo `[alias]` table — built-ins plus user aliases merged across the
    /// hierarchical `.cargo/config.toml` chain.
    CargoAliases,
    /// Go root or `cmd/<name>` package containing `package main`.
    GoPackage,
    /// `bacon.toml` `[jobs.<name>]` tables.
    BaconToml,
    /// `mise.toml` / `.mise.toml` `[tasks.<name>]` tables (and the
    /// inline `[tasks]` flat form).
    MiseToml,
    /// `pyproject.toml` `[project.scripts]` — PEP 621 console-script
    /// entry points, dispatched via the detected Python PM's `run`
    /// (`uv run`, `poetry run`, `pipenv run`).
    PyprojectScripts,
}

/// Expected Node.js version parsed from a version file.
#[derive(Debug, Clone)]
pub(crate) struct NodeVersion {
    /// The version string (e.g. `"20.11.0"`, `">=18"`).
    pub expected: String,
    /// Which file it was read from (e.g. `".nvmrc"`, `"package.json engines"`).
    pub source: &'static str,
}

/// Non-fatal issue found while detecting project metadata or resolving a
/// package manager.
///
/// Carried as a typed variant so the diagnostic surface (`doctor --json`,
/// `--explain`) can attribute each warning to a chain step or detector,
/// and so future filtering (e.g. suppress just `PathProbeFallback`) is
/// trivial. The [`Display`] impl renders the same `"<source>: <detail>"`
/// shape every printer expects, so introducing a new variant doesn't
/// churn output sites.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum DetectionWarning {
    /// Manifest declaration (`packageManager` / `devEngines.packageManager`)
    /// disagrees with the detected lockfile. Declaration wins; the lockfile
    /// is likely stale.
    PmMismatch {
        /// The PM the manifest declared.
        declared: PackageManager,
        /// Which manifest field carried the declaration — `"packageManager"`
        /// or `"devEngines.packageManager"`. `&'static str` so it round-trips
        /// through `Display` and JSON unchanged.
        field: &'static str,
        /// The PM the lockfile points to.
        lockfile: PackageManager,
    },
    /// `devEngines.packageManager` declares a binary that isn't on `PATH`.
    /// `onFail=warn` — dispatch proceeds and will fail at spawn time.
    DevEnginesBinaryMissing {
        /// The declared package manager.
        pm: PackageManager,
    },
    /// `devEngines.packageManager` version range isn't satisfied by the
    /// installed binary. `onFail=warn` — declaration wins.
    DevEnginesVersionMismatch {
        /// The declared package manager.
        pm: PackageManager,
        /// Declared version constraint (as written, e.g. `"^9.0.0"`).
        declared: String,
        /// Actual `--version` output of the installed binary.
        actual: String,
    },
    /// Resolver fell through to `PATH` probe because no declarations or
    /// lockfiles matched. Reports the picked binary plus any others that
    /// were also installed, so the user can spot drift between intent
    /// and environment.
    PathProbeFallback {
        /// Which PM the resolver picked.
        picked: PackageManager,
        /// Ecosystem the probe ran for (Node, Python, …).
        ecosystem: Ecosystem,
        /// Other PMs found on `PATH` that the resolver did not pick.
        others_available: Vec<PackageManager>,
    },
    /// `--fallback npm` (or `RUNNER_FALLBACK=npm`) triggered the legacy
    /// silent default. Surfaces so users aren't surprised by an `npm`
    /// dispatch in a project that has no `npm` signals.
    LegacyNpmFallbackUsed {
        /// Ecosystem the fallback applied to.
        ecosystem: Ecosystem,
    },
    /// Task extraction failed for a source (parse error, IO error).
    /// Detection-side warning; not tied to a resolver chain step.
    TaskListUnreadable {
        /// Source label (`"package.json"`, `"justfile"`, etc.).
        source: &'static str,
        /// Formatted error chain from the failing reader.
        error: String,
    },
    /// `package.json` declared a `packageManager` value that doesn't
    /// name a script-dispatching PM (typo, unsupported ecosystem,
    /// empty version after `@`, etc.). Surfaced so the user sees what
    /// the resolver couldn't honour instead of getting a silent
    /// fall-through to lockfile/PATH probe.
    UnparseablePackageManager {
        /// The raw value as written in `package.json`, verbatim, so
        /// the user can spot their typo without re-reading the file.
        raw: String,
    },
    /// An env-var override (`RUNNER_PM`, `RUNNER_RUNNER`) held a value
    /// that doesn't parse, and the command chose to report it instead
    /// of dying — `runner doctor` must be able to diagnose the broken
    /// environment it exists to diagnose. Strict commands still treat
    /// the same condition as a fatal error.
    InvalidEnvOverride {
        /// The variable that carried the value (`"RUNNER_PM"`).
        var: &'static str,
        /// The offending value, pre-sanitized for display (control
        /// chars escaped, truncated).
        raw: String,
        /// Rendered parse error, already source-prefixed.
        message: String,
    },
    /// Two or more detected package managers install into the same
    /// directory (e.g. `bun` and a `nodeModulesDir`-enabled `deno` both
    /// write `node_modules/`). Running both fans out redundant installs
    /// over a shared tree; restrict the set with `[install].pms`.
    InstallDirCollision {
        /// The shared install directory, e.g. `"node_modules"`.
        dir: &'static str,
        /// The detected PMs that target it, in detection order.
        pms: Vec<PackageManager>,
    },
    /// `runner.toml` carries a key this build doesn't recognize — a typo, or
    /// a section/field added by a newer `runner`. Tolerated for forward
    /// compatibility: the key is ignored and the rest of the config still
    /// applies, so a config written by one version never bricks task
    /// dispatch under another. Surfaced as a warning so genuine typos stay
    /// visible instead of vanishing silently.
    UnknownConfigKey {
        /// Dotted path to the unrecognized key: `"github"` for an unknown
        /// section, `"chain.fast"` for an unknown field within a known one.
        path: String,
    },
    /// `runner.toml` sets a key that still works but has a supported
    /// successor. The deprecated key keeps functioning (unless `superseded`,
    /// in which case the successor it conflicts with takes over) so configs
    /// never break on upgrade; the warning nudges migration.
    DeprecatedConfigKey {
        /// Dotted path to the deprecated key, e.g. `"task_runner.prefer"`.
        path: String,
        /// Dotted path to the replacement, e.g. `"tasks.prefer"`.
        replacement: &'static str,
        /// `true` when the replacement is also set, so the deprecated key is
        /// ignored this run; `false` when the deprecated key is still in effect.
        superseded: bool,
    },
}

impl DetectionWarning {
    /// Subsystem the warning came from, used as the prefix in both the
    /// human renderer (`warn: <source>: <detail>`) and the JSON shape
    /// (`{ "source": "...", "detail": "..." }`). Kept as `&'static str`
    /// so the JSON contract emitted by `doctor --json` stays byte-stable
    /// across the flat-struct → enum refactor.
    pub(crate) const fn source(&self) -> &'static str {
        match self {
            Self::PmMismatch { .. }
            | Self::DevEnginesBinaryMissing { .. }
            | Self::DevEnginesVersionMismatch { .. }
            | Self::UnparseablePackageManager { .. } => "package.json",
            Self::PathProbeFallback { .. } | Self::LegacyNpmFallbackUsed { .. } => "resolver",
            Self::TaskListUnreadable { source, .. } => source,
            Self::InvalidEnvOverride { .. } => "env",
            Self::InstallDirCollision { .. } => "install",
            Self::UnknownConfigKey { .. } | Self::DeprecatedConfigKey { .. } => "runner.toml",
        }
    }

    /// Human-readable detail line. Renders the variant-specific message
    /// without the `<source>:` prefix; pair with [`Self::source`] (or
    /// [`Display`]) to produce the full warning line.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::PmMismatch {
                declared,
                field,
                lockfile,
            } => format!(
                "{field} declares {} but the lockfile reflects {} — declaration wins; regenerate \
                 the lockfile to silence this",
                declared.label(),
                lockfile.label(),
            ),
            Self::DevEnginesBinaryMissing { pm } => format!(
                "devEngines.packageManager declares {} but it was not found on PATH; dispatch \
                 will fail at spawn time",
                pm.label(),
            ),
            Self::DevEnginesVersionMismatch {
                pm,
                declared,
                actual,
            } => format!(
                "devEngines.packageManager requires {} {declared} but the installed version is \
                 {actual}",
                pm.label(),
            ),
            Self::PathProbeFallback {
                picked,
                ecosystem,
                others_available,
            } => {
                let eco = ecosystem.label();
                if others_available.is_empty() {
                    format!(
                        "no {eco} signals matched — using {} from PATH",
                        picked.label(),
                    )
                } else {
                    let others = others_available
                        .iter()
                        .map(|pm| pm.label())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "no {eco} signals matched — using {} from PATH (also available: {others})",
                        picked.label(),
                    )
                }
            }
            Self::LegacyNpmFallbackUsed { ecosystem } => format!(
                "no {} signals matched; using npm via --fallback=npm",
                ecosystem.label(),
            ),
            Self::TaskListUnreadable { error, .. } => format!("failed to read tasks: {error}"),
            Self::UnparseablePackageManager { raw } => format!(
                "packageManager value {raw:?} doesn't name a script-dispatching package manager \
                 (expected one of npm|pnpm|yarn|bun|deno, optionally followed by @<version>); \
                 declaration ignored, falling back to lockfile / PATH probe",
            ),
            Self::InvalidEnvOverride { var, message, .. } => {
                format!("{var} is set but invalid and was ignored for this report: {message}")
            }
            Self::InstallDirCollision { dir, pms } => {
                let pms = pms
                    .iter()
                    .map(|pm| pm.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{pms} all install into {dir}/ — running them together fans out redundant \
                     installs over a shared tree. Restrict the set with `[install].pms` in \
                     runner.toml (or `RUNNER_INSTALL_PMS`).",
                )
            }
            Self::UnknownConfigKey { path } => format!(
                "unknown key `{path}` ignored — a typo, or written by a newer runner. This build \
                 doesn't recognize it; the rest of the config still applies.",
            ),
            Self::DeprecatedConfigKey {
                path,
                replacement,
                superseded,
            } => {
                if *superseded {
                    format!(
                        "`{path}` is deprecated and ignored here because `{replacement}` is also \
                         set; remove `{path}`.",
                    )
                } else {
                    format!(
                        "`{path}` is deprecated; migrate to `{replacement}` (rank-only, and accepts \
                         package managers). It still applies for now.",
                    )
                }
            }
        }
    }
}

impl std::fmt::Display for DetectionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.source(), self.detail())
    }
}

/// Everything detected about the current project directory.
pub(crate) struct ProjectContext {
    /// Absolute path to the project root that was scanned.
    pub root: PathBuf,
    /// Detected package managers, ordered by detection priority.
    pub package_managers: Vec<PackageManager>,
    /// Detected task runners.
    pub task_runners: Vec<TaskRunner>,
    /// All extracted tasks, sorted by source then name.
    pub tasks: Vec<Task>,
    /// Expected Node.js version from `.nvmrc`, `.node-version`, etc.
    pub node_version: Option<NodeVersion>,
    /// Currently installed Node.js version (from `node --version`).
    pub current_node: Option<String>,
    /// Whether the project appears to be a monorepo.
    pub is_monorepo: bool,
    /// Non-fatal detection issues surfaced to task-facing commands.
    pub warnings: Vec<DetectionWarning>,
}

impl ProjectContext {
    /// Returns the first Node-ecosystem package manager, if any.
    pub(crate) fn primary_node_pm(&self) -> Option<PackageManager> {
        self.package_managers
            .iter()
            .copied()
            .find(|pm| pm.is_node())
    }

    /// Returns the first detected package manager of any ecosystem.
    pub(crate) fn primary_pm(&self) -> Option<PackageManager> {
        self.package_managers.first().copied()
    }
}

impl PackageManager {
    /// Returns `true` for Node.js package managers (npm, yarn, pnpm, bun).
    pub(crate) const fn is_node(self) -> bool {
        matches!(self, Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun)
    }

    /// Human-readable CLI name (e.g. `"pnpm"`, `"cargo"`).
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
            Self::Bun => "bun",
            Self::Cargo => "cargo",
            Self::Deno => "deno",
            Self::Uv => "uv",
            Self::Poetry => "poetry",
            Self::Pipenv => "pipenv",
            Self::Go => "go",
            Self::Bundler => "bundler",
            Self::Composer => "composer",
        }
    }

    /// Parse a user-supplied label (CLI flag value, env var, config field)
    /// back into a [`PackageManager`].
    ///
    /// Accepts the canonical `label()` for each variant and the common
    /// `bundle` alias for Ruby's Bundler (which spells its binary `bundle`).
    /// Surrounding whitespace is trimmed so `" pnpm "` from a padded env
    /// var or TOML value still parses; resolver-side parsing also trims
    /// before this is reached but config-loader call sites pass raw
    /// strings.
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label.trim() {
            "npm" => Some(Self::Npm),
            "yarn" => Some(Self::Yarn),
            "pnpm" => Some(Self::Pnpm),
            "bun" => Some(Self::Bun),
            "cargo" => Some(Self::Cargo),
            "deno" => Some(Self::Deno),
            "uv" => Some(Self::Uv),
            "poetry" => Some(Self::Poetry),
            "pipenv" => Some(Self::Pipenv),
            "go" => Some(Self::Go),
            "bundler" | "bundle" => Some(Self::Bundler),
            "composer" => Some(Self::Composer),
            _ => None,
        }
    }

    /// Every variant of [`PackageManager`] in a fixed order, for help text
    /// and error messages that need to enumerate the valid values.
    pub(crate) const fn all() -> &'static [Self] {
        &[
            Self::Npm,
            Self::Yarn,
            Self::Pnpm,
            Self::Bun,
            Self::Cargo,
            Self::Deno,
            Self::Uv,
            Self::Poetry,
            Self::Pipenv,
            Self::Go,
            Self::Bundler,
            Self::Composer,
        ]
    }

    /// Total number of [`PackageManager`] variants. Used as the array
    /// length for static lookup tables keyed on the discriminant.
    pub(crate) const COUNT: usize = 12;

    /// Stable `0..COUNT` index used by static arrays keyed on the
    /// discriminant. Hand-rolled (not `self as usize`) so reordering the
    /// `enum` definition is a compile error rather than silent corruption
    /// of any table indexed by this method.
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Npm => 0,
            Self::Yarn => 1,
            Self::Pnpm => 2,
            Self::Bun => 3,
            Self::Cargo => 4,
            Self::Deno => 5,
            Self::Uv => 6,
            Self::Poetry => 7,
            Self::Pipenv => 8,
            Self::Go => 9,
            Self::Bundler => 10,
            Self::Composer => 11,
        }
    }

    /// The ecosystem this package manager belongs to.
    pub(crate) const fn ecosystem(self) -> Ecosystem {
        match self {
            Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun => Ecosystem::Node,
            Self::Deno => Ecosystem::Deno,
            Self::Cargo => Ecosystem::Rust,
            Self::Uv | Self::Poetry | Self::Pipenv => Ecosystem::Python,
            Self::Go => Ecosystem::Go,
            Self::Bundler => Ecosystem::Ruby,
            Self::Composer => Ecosystem::Php,
        }
    }

    /// Whether this PM can dispatch a script declared in `package.json`
    /// `"scripts"` — Node ecosystem (`npm`, `yarn`, `pnpm`, `bun`) plus
    /// Deno (via `deno run <task>`). Used by both the resolver (to
    /// scope `--pm` overrides for Node-script resolution) and the
    /// bun-test fallback path (to answer "did the user pick a
    /// Node-script PM other than Bun?").
    pub(crate) const fn can_dispatch_node_scripts(self) -> bool {
        self.is_node() || matches!(self, Self::Deno)
    }

    /// The task source(s) this package manager runs natively, most-native
    /// first. A forced `--pm` / `RUNNER_PM` biases same-name task selection
    /// toward these, in order, so the chosen task dispatches through the PM
    /// the user asked for instead of being run *through* it from a foreign
    /// source (e.g. `RUNNER_PM=deno run check` picks the `deno.json` task
    /// over a same-named `package.json` script).
    ///
    /// npm/yarn/pnpm/bun all run `package.json` `"scripts"`. Deno is
    /// dual-natured: it owns `deno.json` tasks (`deno task`) *and* also runs
    /// `package.json` scripts, so it prefers the former and falls back to
    /// the latter. Cargo, Go, and the Python PMs own their ecosystem's
    /// source. Bundler and Composer have no task source modeled yet, so
    /// they bias nothing. Deno is one member of this rule, not a special
    /// case — the bias is general across every PM.
    pub(crate) const fn owned_task_sources(self) -> &'static [TaskSource] {
        match self {
            Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun => &[TaskSource::PackageJson],
            Self::Deno => &[TaskSource::DenoJson, TaskSource::PackageJson],
            Self::Cargo => &[TaskSource::CargoAliases],
            Self::Go => &[TaskSource::GoPackage],
            Self::Uv | Self::Poetry | Self::Pipenv => &[TaskSource::PyprojectScripts],
            Self::Bundler | Self::Composer => &[],
        }
    }
}

impl TaskRunner {
    /// Human-readable CLI name (e.g. `"turbo"`, `"just"`).
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Turbo => "turbo",
            Self::Nx => "nx",
            Self::Make => "make",
            Self::Just => "just",
            Self::GoTask => "task",
            Self::Mise => "mise",
            Self::Bacon => "bacon",
        }
    }

    /// Parse a user-supplied label (CLI flag value, env var, config field)
    /// back into a [`TaskRunner`].
    ///
    /// Accepts the canonical `label()` plus the alias `go-task` for `task`
    /// to disambiguate from arbitrary task names. Surrounding whitespace is
    /// trimmed to match [`PackageManager::from_label`].
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label.trim() {
            "turbo" => Some(Self::Turbo),
            "nx" => Some(Self::Nx),
            "make" => Some(Self::Make),
            "just" => Some(Self::Just),
            "task" | "go-task" => Some(Self::GoTask),
            "mise" => Some(Self::Mise),
            "bacon" => Some(Self::Bacon),
            _ => None,
        }
    }

    /// Every variant of [`TaskRunner`] in a fixed order, for help text and
    /// error messages that need to enumerate the valid values.
    pub(crate) const fn all() -> &'static [Self] {
        &[
            Self::Turbo,
            Self::Nx,
            Self::Make,
            Self::Just,
            Self::GoTask,
            Self::Mise,
            Self::Bacon,
        ]
    }

    /// The [`TaskSource`] that holds this runner's tasks, when extraction
    /// is implemented for it. Used by completion to dedupe `package.json`
    /// passthrough wrappers against the underlying runner's task entry.
    ///
    /// Returns `None` for runners where task extraction is not yet
    /// implemented (Nx); a passthrough wrapper still routes to that
    /// runner at dispatch time, but completion shows the script as its
    /// own candidate because there is no peer entry to collapse it into.
    pub(crate) const fn task_source(self) -> Option<TaskSource> {
        match self {
            Self::Turbo => Some(TaskSource::TurboJson),
            Self::Make => Some(TaskSource::Makefile),
            Self::Just => Some(TaskSource::Justfile),
            Self::GoTask => Some(TaskSource::Taskfile),
            Self::Bacon => Some(TaskSource::BaconToml),
            Self::Mise => Some(TaskSource::MiseToml),
            Self::Nx => None,
        }
    }
}

impl TaskSource {
    /// Every task source in display order. Used by renderers and error
    /// messages so adding a source updates diagnostics from one place.
    pub(crate) const fn all() -> &'static [Self] {
        &[
            Self::PackageJson,
            Self::Makefile,
            Self::Justfile,
            Self::Taskfile,
            Self::TurboJson,
            Self::DenoJson,
            Self::CargoAliases,
            Self::GoPackage,
            Self::BaconToml,
            Self::MiseToml,
            Self::PyprojectScripts,
        ]
    }

    /// Canonical display label shown to the user — the *tool* name where a
    /// single tool owns the source (`"make"`, `"just"`, `"bacon"`, …), or
    /// the filename when multiple tools share the source (`"package.json"`
    /// is read by npm/yarn/pnpm/bun, so there's no single owner to name).
    ///
    /// Previously a mix of tool names (`"cargo"`) and filenames
    /// (`"bacon.toml"`, `"turbo.json"`); the inconsistency made the
    /// `runner list` column read like a typo. Standardizing on tool
    /// names also stops cases like `bacon.toml` claiming jobs that
    /// actually come from `~/.config/bacon/prefs.toml` — the label
    /// "bacon" is honest about that breadth, the label "bacon.toml"
    /// isn't.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::PackageJson => "package.json",
            Self::Makefile => "make",
            Self::Justfile => "just",
            Self::Taskfile => "task",
            Self::TurboJson => "turbo",
            Self::DenoJson => "deno",
            // Synthetic source — aliases merge across the hierarchical
            // `.cargo/config.toml` chain plus `$CARGO_HOME`, so no single
            // file name represents it.
            Self::CargoAliases => "cargo",
            Self::GoPackage => "go",
            Self::BaconToml => "bacon",
            Self::MiseToml => "mise",
            // Filename, not a tool name: `[project.scripts]` is read by
            // uv, poetry, and pipenv alike, so no single tool owns it.
            Self::PyprojectScripts => "pyproject.toml",
        }
    }

    /// Parse a source label back to a [`TaskSource`]. Accepts both the
    /// current canonical [`label`]s and the older filename-style labels
    /// (`"justfile"`, `"bacon.toml"`, `"turbo.json"`, …) so qualified
    /// task syntax (`bacon.toml:check`) that users may have in shell
    /// history, scripts, or muscle memory keeps working unchanged.
    ///
    /// [`label`]: TaskSource::label
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label {
            "package.json" => Some(Self::PackageJson),
            "make" | "Makefile" => Some(Self::Makefile),
            "just" | "justfile" => Some(Self::Justfile),
            "task" | "Taskfile" | "go-task" => Some(Self::Taskfile),
            "turbo" | "turbo.json" | "turbo.jsonc" => Some(Self::TurboJson),
            "deno" | "deno.json" | "deno.jsonc" => Some(Self::DenoJson),
            "cargo" => Some(Self::CargoAliases),
            "go" | "go.mod" => Some(Self::GoPackage),
            "bacon" | "bacon.toml" => Some(Self::BaconToml),
            "mise" | "mise.toml" | ".mise.toml" => Some(Self::MiseToml),
            "pyproject" | "pyproject.toml" => Some(Self::PyprojectScripts),
            _ => None,
        }
    }

    /// Display order for grouped task listings.
    pub(crate) const fn display_order(self) -> u8 {
        match self {
            Self::PackageJson => 0,
            Self::Makefile => 1,
            Self::Justfile => 2,
            Self::Taskfile => 3,
            Self::TurboJson => 4,
            Self::DenoJson => 5,
            Self::CargoAliases => 6,
            Self::GoPackage => 7,
            Self::BaconToml => 8,
            Self::MiseToml => 9,
            Self::PyprojectScripts => 10,
        }
    }
}

/// Does `current` satisfy the `expected` version constraint?
///
/// `expected` accepts the node-semver range grammar found in `.nvmrc`,
/// `.node-version`, `.tool-versions`, and `package.json` `engines.node`:
/// comparator sets (`>=22.22.2`, `>=18 <21`), caret/tilde ranges
/// (`^20.11`, `~18.15`), `||` unions, hyphen ranges (`18 - 20`), and
/// wildcards (`20.x`). Evaluation is delegated to the `semver` crate
/// after normalizing node's grammar into the comma-separated comparator
/// form it parses.
///
/// Bare versions (`"20"`, `"20.11"`) keep prefix-at-segment-boundary
/// semantics — a `.nvmrc` saying `20.11` means "any 20.11.x", which is
/// narrower than the caret default the `semver` crate would apply.
///
/// Anything unevaluable (`lts/*`, malformed ranges, a non-version
/// `current`) falls back to the historical prefix match, so this never
/// panics and never rejects input it used to accept.
///
/// A prerelease `current` (e.g. `23.0.0-nightly`) only matches a
/// comparator that pins the same triple with a prerelease tag — the
/// `semver` crate's gate, mirroring node-semver's default behavior.
pub(crate) fn version_matches(expected: &str, current: &str) -> bool {
    let expected = expected.trim();
    let current = current.trim();

    if bare_version(expected) {
        return prefix_version_matches(expected, current);
    }
    range_matches(expected, current).unwrap_or_else(|| prefix_version_matches(expected, current))
}

/// The historical loose prefix match, kept as the fallback for inputs
/// the range path can't evaluate and as the primary semantics for bare
/// versions.
///
/// Strips leading range operators (`>=`, `~`, `^`, etc.) and checks
/// whether `current` starts with the cleaned `expected` value at a
/// segment boundary. A bare major version like `"20"` matches `"20.x.y"`.
fn prefix_version_matches(expected: &str, current: &str) -> bool {
    let after_ops = expected
        .trim()
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('=')
        .trim_start_matches('~')
        .trim_start_matches('^')
        .trim_start();
    let expected_clean = strip_v(after_ops).trim();

    current.starts_with(expected_clean)
        && current[expected_clean.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == '.')
}

/// True when `s` is a plain version literal — optionally `v`-prefixed,
/// then nothing but ASCII digits and dots (`20`, `20.11`, `v20.11.0`).
/// Operators, wildcards, and named aliases (`lts/*`) all return false.
fn bare_version(s: &str) -> bool {
    let stripped = strip_v(s);
    !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Strip a single leading `v` (`v18` → `18`) per nvm/Corepack convention.
fn strip_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

/// Evaluate `expected` as a node-semver range against `current`.
///
/// Returns `None` when the outcome could not be determined — `current`
/// isn't a version, or no `||` group both parsed and matched while at
/// least one group was unparseable — so the caller can fall back to the
/// prefix match. A parsed-and-matching group wins immediately, letting
/// `">=18 || lts/*"` succeed on the evaluable half.
fn range_matches(expected: &str, current: &str) -> Option<bool> {
    let cur = parse_current_version(current)?;
    let mut any_unparseable = false;
    for group in expected.split("||") {
        let group = group.trim();
        if group.is_empty() {
            any_unparseable = true;
            continue;
        }
        let req = normalize_range_group(group)
            .and_then(|normalized| semver::VersionReq::parse(&normalized).ok());
        match req {
            Some(req) if req.matches(&cur) => return Some(true),
            Some(_) => {}
            None => any_unparseable = true,
        }
    }
    if any_unparseable { None } else { Some(false) }
}

/// Rewrite one `||`-free node-semver comparator group into the
/// comma-separated grammar `semver::VersionReq::parse` accepts.
///
/// Handles hyphen ranges (`18 - 20` → `>=18, <=20`; a partial upper
/// bound is already inclusive of its whole segment in the crate's
/// grammar), whitespace-separated AND comparators, operators detached
/// from their version (`>= 18`), and per-token `v` prefixes. Bare
/// digit-leading tokens get an `=` operator — the crate would otherwise
/// default them to caret, which is looser than node's exact-partial
/// semantics. Wildcard tokens (`*`, `x`) pass through untouched because
/// `=*` does not parse.
fn normalize_range_group(group: &str) -> Option<String> {
    let group = group.replace(',', " ");
    let tokens: Vec<&str> = group.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    if let [low, "-", high] = tokens.as_slice() {
        return Some(format!(">={}, <={}", strip_v(low), strip_v(high)));
    }
    if tokens.contains(&"-") {
        return None;
    }

    let mut parts: Vec<String> = Vec::with_capacity(tokens.len());
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        let (op, rest) = split_operator(token);
        if op.is_empty() {
            let rest = strip_v(rest);
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                parts.push(format!("={rest}"));
            } else {
                parts.push(rest.to_string());
            }
        } else if rest.is_empty() {
            let version = iter.next()?;
            parts.push(format!("{op}{}", strip_v(version)));
        } else {
            parts.push(format!("{op}{}", strip_v(rest)));
        }
    }
    Some(parts.join(", "))
}

/// Split a leading range operator off a comparator token. Returns
/// `(op, rest)` with `op` ∈ {`>=`, `<=`, `>`, `<`, `=`, `~`, `^`, ``""``}.
fn split_operator(token: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "=", "~", "^"] {
        if let Some(rest) = token.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", token)
}

/// Parse `current` (a `node --version`-style string with the `v`
/// already stripped by detection) into a full [`semver::Version`],
/// padding bare `major`/`major.minor` forms to a triple. Deliberately
/// duplicates the padding in `tool::node::normalize_version` — `types`
/// must not grow a dependency on `tool`.
fn parse_current_version(current: &str) -> Option<semver::Version> {
    let padded = match current.split('.').count() {
        1 => format!("{current}.0.0"),
        2 => format!("{current}.0"),
        _ => current.to_string(),
    };
    semver::Version::parse(&padded).ok()
}

#[cfg(test)]
mod tests {
    use super::version_matches;
    use super::{DetectionWarning, PackageManager};

    #[test]
    fn dotted_versions_match_segment_boundaries_only() {
        assert!(version_matches("20.11", "20.11.0"));
        assert!(!version_matches("20.11", "20.110.0"));
    }

    #[test]
    fn gte_range_matches_higher_versions() {
        // Regression: ">=22.22.2" used to prefix-match as "=22.22.2",
        // warning on 22.22.3 and 25.9.0 — both satisfy the range.
        assert!(version_matches(">=22.22.2", "22.22.3"));
        assert!(version_matches(">=22.22.2", "25.9.0"));
        assert!(!version_matches(">=22.22.2", "22.22.1"));
    }

    #[test]
    fn operator_with_space_before_version() {
        assert!(version_matches(">= 18", "20.0.0"));
        assert!(!version_matches(">= 18", "17.9.0"));
    }

    #[test]
    fn partial_comparator_bounds() {
        assert!(version_matches(">=18", "18.0.0"));
        // node desugars ">22" to ">=23.0.0": 22.x never qualifies.
        assert!(!version_matches(">22", "22.5.0"));
        assert!(version_matches(">22", "23.0.0"));
        assert!(version_matches("<21", "20.99.0"));
        assert!(!version_matches("<21", "21.0.0"));
        assert!(version_matches("<=20", "20.99.0"));
    }

    #[test]
    fn caret_ranges() {
        // The case bare-prefix semantics must reject but caret accepts.
        assert!(version_matches("^20.11", "20.12.0"));
        assert!(!version_matches("^20.11", "20.10.9"));
        assert!(!version_matches("^20.11", "21.0.0"));
        assert!(version_matches("^0.3", "0.3.9"));
        assert!(!version_matches("^0.3", "0.4.0"));
    }

    #[test]
    fn tilde_ranges() {
        assert!(version_matches("~18.15", "18.15.7"));
        assert!(!version_matches("~18.15", "18.16.0"));
        assert!(version_matches("~18.15.0", "18.15.3"));
    }

    #[test]
    fn space_separated_and_conjunction() {
        assert!(version_matches(">=18 <21", "20.5.1"));
        assert!(!version_matches(">=18 <21", "21.0.0"));
        assert!(!version_matches(">=18 <21", "17.0.0"));
    }

    #[test]
    fn or_unions() {
        assert!(version_matches("18||20", "20.4.2"));
        assert!(!version_matches("18||20", "19.0.0"));
        assert!(version_matches(">=18 <19 || >=20", "18.5.0"));
        assert!(!version_matches(">=18 <19 || >=20", "19.5.0"));
        assert!(version_matches(">=18 <19 || >=20", "25.9.0"));
    }

    #[test]
    fn hyphen_ranges() {
        assert!(version_matches("18 - 20", "19.0.0"));
        // Inclusive partial upper bound: node treats "- 20" as "<21".
        assert!(version_matches("18 - 20", "20.9.9"));
        assert!(!version_matches("18 - 20", "21.0.0"));
        assert!(!version_matches("18 - 20", "17.9.9"));
    }

    #[test]
    fn wildcard_ranges() {
        assert!(version_matches("20.x", "20.5.1"));
        assert!(!version_matches("20.x", "21.0.0"));
        assert!(version_matches("20.*", "20.0.0"));
        assert!(version_matches("*", "99.0.0"));
    }

    #[test]
    fn bare_versions_keep_prefix_semantics() {
        // Regression guard for the caret trap: the semver crate would
        // read a bare "20.11" as "^20.11" and accept 20.12.
        assert!(!version_matches("20.11", "20.12.0"));
        assert!(version_matches("20", "20.11.0"));
        assert!(!version_matches("2", "20.11.0"));
        assert!(version_matches("v20", "20.1.0"));
        assert!(version_matches("20.11.0", "20.11.0"));
    }

    #[test]
    fn exact_operator_partial_equality() {
        assert!(version_matches("=20.11", "20.11.5"));
        assert!(!version_matches("=20.11", "20.12.0"));
    }

    #[test]
    fn operator_with_v_prefix() {
        assert!(version_matches(">=v18", "18.0.0"));
    }

    #[test]
    fn unparseable_expected_falls_back_to_prefix() {
        assert!(!version_matches("lts/*", "22.0.0"));
        assert!(!version_matches("lts/jod", "22.0.0"));
        assert!(!version_matches("", "20.0.0"));
    }

    #[test]
    fn unparseable_or_group_does_not_block_parsed_match() {
        assert!(version_matches(">=18 || lts/*", "20.0.0"));
    }

    #[test]
    fn unparseable_current_falls_back_to_prefix() {
        assert!(!version_matches(">=18", "not-a-version"));
    }

    #[test]
    fn prefix_fallback_strips_equals_and_spaced_v() {
        // An unparseable `current` forces the prefix fallback; the
        // cleaned expected value must survive a bare `=` operator and
        // whitespace between the operator and a `v`-prefixed version.
        assert!(version_matches("=20.11", "20.11.beta"));
        assert!(!version_matches("=20.11", "20.12.beta"));
        assert!(version_matches(">= v18", "18.unknown"));
    }

    #[test]
    fn detection_warning_can_be_hashed() {
        use std::collections::HashSet;

        let a = DetectionWarning::DevEnginesBinaryMissing {
            pm: PackageManager::Pnpm,
        };
        let b = DetectionWarning::DevEnginesBinaryMissing {
            pm: PackageManager::Pnpm,
        };
        let c = DetectionWarning::DevEnginesBinaryMissing {
            pm: PackageManager::Yarn,
        };

        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        set.insert(c);

        assert_eq!(set.len(), 2, "equal variants should dedup");
    }
}
