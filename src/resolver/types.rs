//! Resolver data types — the public structs/enums + their trivial impls.
//!
//! No resolution logic, no parsing — just the shapes the rest of the
//! resolver passes around. `impl Resolver` lives in [`super::resolve`];
//! `impl ResolutionOverrides` lives in [`super::overrides`].

use std::collections::HashMap;
use std::path::PathBuf;

use crate::chain::FailurePolicy;
use crate::config::LoadedConfig;
use crate::tool::node::OnFail;
use crate::types::{DetectionWarning, Ecosystem, PackageManager, ProjectContext, TaskRunner};

/// Resolves package managers and task sources from a [`ProjectContext`]
/// plus a bundle of [`ResolutionOverrides`].
pub(crate) struct Resolver<'ctx> {
    pub(super) ctx: &'ctx ProjectContext,
    pub(super) overrides: &'ctx ResolutionOverrides,
}

/// User-supplied overrides assembled from CLI flags, environment variables,
/// and (Phase 3+) a `runner.toml` file.
///
/// Each field carries an [`OverrideOrigin`] so diagnostic output (Phase 6)
/// can attribute a decision to the exact source the user set it from.
#[derive(Debug, Clone, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "settings bag of independent CLI/env/config toggles; an enum state machine would \
              obscure them, not clarify"
)]
pub(crate) struct ResolutionOverrides {
    /// Cross-ecosystem PM override from CLI/env. `--pm`/`RUNNER_PM` are not
    /// ecosystem-qualified; the resolver applies this value only when the
    /// named PM is compatible with the requested ecosystem.
    pub pm: Option<PmOverride>,
    /// Per-ecosystem PM overrides from `runner.toml`. Consulted after the
    /// cross-ecosystem CLI/env override falls through (e.g. `--pm cargo`
    /// against a Node resolution).
    pub pm_by_ecosystem: HashMap<Ecosystem, PmOverride>,
    /// Task-runner override from `--runner` / `RUNNER_RUNNER`. When set,
    /// the source selector restricts candidates to that runner's
    /// [`TaskRunner::task_source`]; an empty restriction list bails with
    /// [`super::ResolveError::InvalidOverride`].
    pub runner: Option<RunnerOverride>,
    /// Ranked preference list from `[task_runner].prefer`. Empty when no
    /// config is loaded or the section is empty. When non-empty, the
    /// source selector restricts candidates to runners in the list (in
    /// listed order); a miss bails with [`super::ResolveError::InvalidOverride`].
    pub prefer_runners: Vec<TaskRunner>,
    /// What to do when no signal in steps 2–6 matches.
    pub fallback: FallbackPolicy,
    /// What to do when the manifest declaration (step 5) disagrees with
    /// the detected lockfile (step 6).
    pub on_mismatch: MismatchPolicy,
    /// When `true`, suppress all `DetectionWarning` output. Set via
    /// `--no-warnings` / `RUNNER_NO_WARNINGS`. Errors still surface;
    /// only non-fatal warnings are silenced.
    pub no_warnings: bool,
    /// When `true`, suppress the dispatch arrow (`→ <source> <task>`) and
    /// the dispatch-time `--explain` trace on stderr. Set via `--quiet` /
    /// `RUNNER_QUIET`.
    pub quiet: bool,
    /// When `true`, emit a one-line trace describing which chain step
    /// produced the PM decision. Set via `--explain` / `RUNNER_EXPLAIN`.
    pub explain: bool,
    /// Failure policy for `run -s/-p` chains and `runner install <tasks>`.
    /// Resolved from `-k`/`-K` (CLI) → `RUNNER_KEEP_GOING`/`RUNNER_KILL_ON_FAIL`
    /// (env) → `[chain]` (config) → `FailFast`.
    pub failure_policy: FailurePolicy,
    /// Broad GitHub Actions grouping switch. Sourced from
    /// `[github].group_output` (default `true`); when false, GitHub Actions
    /// runs use the same ungrouped output shape as before this feature.
    pub group_output: bool,
    /// Whether to group parallel (`-p`) output **under GitHub Actions**:
    /// buffer each task and print it as one block on completion rather than
    /// interleaving lines live. Sourced from `[github].group_parallel`
    /// (default `true`) and only active when [`Self::group_output`] is true.
    /// The emit site picks this under GitHub Actions and
    /// [`Self::parallel_grouped`] otherwise.
    pub github_group_parallel: bool,
    /// Whether to group parallel (`-p`) output **outside GitHub Actions**.
    /// Sourced from `[parallel].grouped` (default `false`), so by default
    /// local parallel runs stay live-prefixed while CI groups; set them to
    /// match if desired. Only the delimiter style (`::group::` vs a plain
    /// header) further depends on the environment.
    pub parallel_grouped: bool,
    /// Allowlist of package managers `runner install` may run, resolved
    /// from `RUNNER_INSTALL_PMS` (env) → `[install].pms` (config). Empty
    /// means "no install filter" — install fans out to every detected PM.
    /// Unlike [`Self::pm`], this never affects script dispatch.
    pub install_pms: Vec<PackageManager>,
}

/// What to do when no signal in steps 2–6 matches.
///
/// Set via `--fallback` / `RUNNER_FALLBACK` / `[resolution].fallback`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

/// How to react when manifest declaration (step 5) and lockfile (step 6)
/// disagree about which package manager the project uses.
///
/// Set via `--on-mismatch` / `RUNNER_ON_MISMATCH` /
/// `[resolution].on_mismatch`. Independent from
/// `devEngines.packageManager` `onFail` — that policy governs whether
/// the *declared* PM can actually run; this one governs whether the
/// resolver tolerates the declaration disagreeing with the install
/// state at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum MismatchPolicy {
    /// Emit a `package.json` warning, prefer the declaration (Corepack
    /// semantics — the lockfile is most likely stale).
    #[default]
    Warn,
    /// Stay silent; prefer the declaration.
    Ignore,
    /// Bail with [`super::ResolveError::MismatchPolicyError`]. Intended for
    /// CI guardrails where a mismatch should block the run.
    Error,
}

/// A package-manager override plus the source the user set it from.
#[derive(Debug, Clone)]
pub(crate) struct PmOverride {
    /// The chosen package manager.
    pub pm: PackageManager,
    /// Where the override came from.
    pub origin: OverrideOrigin,
}

/// A task-runner override plus the source the user set it from.
#[derive(Debug, Clone)]
pub(crate) struct RunnerOverride {
    /// The chosen task runner.
    pub runner: TaskRunner,
    /// Where the override came from. Surfaced by `--explain` and `doctor`
    /// so the user can attribute the constraint to its origin.
    #[allow(
        dead_code,
        reason = "consumed by --explain in Phase 6; kept on the type for future trace renderers"
    )]
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
        /// Absolute path the override was loaded from. Surfaced by
        /// `ResolvedPm::describe` (which feeds `--explain` and the
        /// `doctor` trace) so the user can attribute a decision to the
        /// exact config file it came from.
        path: PathBuf,
    },
}

impl OverrideOrigin {
    /// Render the "via …" provenance fragment for a PM override from
    /// this origin: `via --pm (CLI override)`, `via RUNNER_PM
    /// (environment)`, or `via runner.toml at <path>`. Shared by
    /// [`ResolvedPm::describe`] and install's override errors so the
    /// attribution wording stays identical everywhere.
    pub(crate) fn describe_pm_source(&self) -> String {
        match self {
            Self::CliFlag => "via --pm (CLI override)".to_string(),
            Self::EnvVar => "via RUNNER_PM (environment)".to_string(),
            Self::ConfigFile { path } => format!("via runner.toml at {}", path.display()),
        }
    }
}

/// A package-manager decision plus the chain step that produced it.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPm {
    /// The chosen package manager.
    pub pm: PackageManager,
    /// Which step of the resolution chain produced [`Self::pm`].
    /// Surfaced by [`Self::describe`] for `--explain` and the
    /// `doctor` / `why` traces.
    pub via: ResolutionStep,
    /// Non-fatal warnings emitted while resolving — e.g. a manifest
    /// declaration that disagrees with the detected lockfile.
    pub warnings: Vec<DetectionWarning>,
}

/// Which step of the resolution chain produced a decision.
///
/// Listed in precedence order. Downstream `match` sites stay exhaustive so
/// that adding a step is a compile error to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolutionStep {
    /// Steps 2–4 — user-supplied override won.
    Override(OverrideOrigin),
    /// Step 5a — `package.json` legacy `packageManager` field.
    ManifestPackageManager,
    /// Step 5b — `package.json` `devEngines.packageManager` field.
    ManifestDevEngines {
        /// Effective `onFail` value for the chosen entry. Rendered into
        /// the `--explain` / `doctor` trace via [`ResolvedPm::describe`].
        on_fail: OnFail,
    },
    /// Step 6 — package manager inferred from a lockfile (or another
    /// detector recorded in [`ProjectContext::package_managers`]).
    Lockfile,
    /// Step 7 — discovered via `$PATH` probe in canonical order.
    PathProbe {
        /// Absolute path of the executable found on PATH. Rendered by
        /// [`ResolvedPm::describe`] so the user can spot which directory
        /// the resolver fell back to.
        binary: PathBuf,
    },
    /// Step 8 (legacy) — no signals matched, default to `npm` so that
    /// `runner run <script>` still has a chance to dispatch. The Phase 5
    /// default replaces this with a [`Self::PathProbe`]; this variant only
    /// fires with `--fallback npm`.
    LegacyNpmFallback,
}

/// Sources contributing to a [`ResolutionOverrides`].
///
/// Bundles every CLI/env input the resolver consumes so
/// `ResolutionOverrides::from_sources` stays extensible — adding a new
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
    /// `--fallback` flag value plus `RUNNER_FALLBACK` env.
    pub fallback: SourceValue<'a>,
    /// `--on-mismatch` flag value plus `RUNNER_ON_MISMATCH` env.
    pub on_mismatch: SourceValue<'a>,
    /// `--no-warnings` flag presence plus `RUNNER_NO_WARNINGS` env.
    pub no_warnings: ExplainSource<'a>,
    /// `-q`/`--quiet` flag presence plus `RUNNER_QUIET` env.
    pub quiet: ExplainSource<'a>,
    /// `--explain` flag presence plus `RUNNER_EXPLAIN` env.
    pub explain: ExplainSource<'a>,
    /// `-k`/`--keep-going` flag presence plus `RUNNER_KEEP_GOING` env.
    pub keep_going: ExplainSource<'a>,
    /// `--kill-on-fail` flag presence plus `RUNNER_KILL_ON_FAIL` env.
    pub kill_on_fail: ExplainSource<'a>,
    /// `RUNNER_INSTALL_PMS` env (comma/space-separated). No CLI flag; the
    /// config side comes from the loaded `runner.toml` `[install].pms`.
    pub install_pms: SourceValue<'a>,
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

/// CLI-side diagnostic flags (`--no-warnings`, `--quiet`, `--explain`)
/// bundled into a single struct so `ResolutionOverrides::from_cli_and_env`
/// stays under clippy's argument/bool thresholds.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct DiagnosticFlags {
    /// `--no-warnings` flag presence (CLI side only — env handled inside
    /// `from_cli_and_env`).
    pub no_warnings: bool,
    /// `-q`/`--quiet` flag presence (CLI side only — env handled inside
    /// `from_cli_and_env`).
    pub quiet: bool,
    /// `--explain` flag presence (CLI side only — env handled inside
    /// `from_cli_and_env`).
    pub explain: bool,
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

impl ResolvedPm {
    /// Render a one-line description of the chain step that produced this
    /// decision. Used by `--explain` to attribute the PM choice.
    pub(crate) fn describe(&self) -> String {
        match &self.via {
            ResolutionStep::Override(origin) => {
                format!("{} {}", self.pm.label(), origin.describe_pm_source())
            }
            ResolutionStep::ManifestPackageManager => {
                format!("{} via package.json \"packageManager\"", self.pm.label())
            }
            ResolutionStep::ManifestDevEngines { on_fail } => format!(
                "{} via package.json \"devEngines.packageManager\" (onFail={on_fail:?})",
                self.pm.label(),
            ),
            ResolutionStep::Lockfile => {
                format!("{} via detected lockfile", self.pm.label())
            }
            ResolutionStep::PathProbe { binary } => {
                format!("{} via PATH probe at {}", self.pm.label(), binary.display())
            }
            ResolutionStep::LegacyNpmFallback => {
                format!("{} via --fallback=npm (legacy)", self.pm.label())
            }
        }
    }
}
