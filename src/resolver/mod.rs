//! Resolution of package managers and task sources for `runner run`.
//!
//! The resolver consumes a [`ProjectContext`] (signals discovered during
//! detection) plus a [`ResolutionOverrides`] bundle (CLI flags, env vars,
//! and — in later phases — a `runner.toml`) and returns a single decision
//! tagged with the chain step that produced it.
//!
//! Chain order (lower wins):
//!
//! 1. Qualified syntax (`turbo.json:build`) — handled in `cmd::run` today.
//! 2. CLI flag (`--pm`, `--runner`).
//! 3. Environment variable (`RUNNER_PM`, `RUNNER_RUNNER`).
//! 4. Project config (`./runner.toml`) — Phase 3.
//! 5. Manifest declaration (`packageManager`, `devEngines.packageManager`)
//!    — Phase 4.
//! 6. Lockfile (current behavior; lives in [`crate::detect`]).
//! 7. `PATH` probe in canonical order — Phase 5.
//! 8. Terminal — error with actionable guidance — Phase 5.

mod error;
mod probe;

pub(crate) use error::{DevEnginesFailReason, ResolveError};
/// Re-export of the pure-function probe variant for the `doctor`
/// subcommand. Lets `cmd::doctor` exercise the same PATH walk the
/// resolver uses without owning the env-reading logic.
pub(crate) use probe::probe_in as probe_path_for_doctor;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::config::{LoadedConfig, parse_node_pm, parse_python_pm};
use crate::tool::node::{
    ManifestPmDecl, ManifestSource, OnFail, VersionCheck, check_version_constraint,
    detect_pm_from_manifest,
};
use crate::types::{DetectionWarning, Ecosystem, PackageManager, ProjectContext, TaskRunner};

/// Resolves package managers and task sources from a [`ProjectContext`]
/// plus a bundle of [`ResolutionOverrides`].
pub(crate) struct Resolver<'ctx> {
    ctx: &'ctx ProjectContext,
    overrides: &'ctx ResolutionOverrides,
}

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
    /// Per-ecosystem PM overrides from `runner.toml`. Consulted after the
    /// cross-ecosystem CLI/env override falls through (e.g. `--pm cargo`
    /// against a Node resolution).
    pub pm_by_ecosystem: HashMap<Ecosystem, PmOverride>,
    /// Task-runner override from `--runner` / `RUNNER_RUNNER`. When set,
    /// the source selector restricts candidates to that runner's
    /// [`TaskRunner::task_source`]; an empty restriction list bails with
    /// [`ResolveError::InvalidOverride`].
    pub runner: Option<RunnerOverride>,
    /// Ranked preference list from `[task_runner].prefer`. Empty when no
    /// config is loaded or the section is empty. When non-empty, the
    /// source selector restricts candidates to runners in the list (in
    /// listed order); a miss bails with [`ResolveError::InvalidOverride`].
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
    /// When `true`, emit a one-line trace describing which chain step
    /// produced the PM decision. Set via `--explain` / `RUNNER_EXPLAIN`.
    pub explain: bool,
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
    /// Bail with [`ResolveError::MismatchPolicyError`]. Intended for
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

impl<'ctx> Resolver<'ctx> {
    /// Wrap a project context plus the override bundle for this invocation.
    pub(crate) const fn new(
        ctx: &'ctx ProjectContext,
        overrides: &'ctx ResolutionOverrides,
    ) -> Self {
        Self { ctx, overrides }
    }

    /// Resolve the package manager used to dispatch `package.json` scripts.
    ///
    /// Walks the precedence chain in order:
    /// - Step 2–3 — CLI/env PM override (when compatible with Node scripts).
    /// - Step 4 — `runner.toml` `[pm].node` override.
    /// - Step 5a — `package.json` legacy `packageManager` field.
    /// - Step 5b — `package.json` `devEngines.packageManager` field
    ///   (honoring `onFail` when the declared PM is missing from PATH).
    /// - Step 6 — lockfile (via [`ProjectContext::primary_node_pm`]).
    /// - Step 7 — `$PATH` probe in canonical Node order
    ///   (`bun > pnpm > yarn > npm`). Active by default; replaced by
    ///   step 8 when `--fallback npm` is set.
    /// - Step 8 — error or legacy `npm` (depending on
    ///   [`FallbackPolicy`]).
    ///
    /// When a manifest declaration (step 5) disagrees with a detected
    /// lockfile (step 6), the manifest wins (Corepack semantics) and a
    /// `package.json` warning is emitted.
    ///
    /// # Errors
    ///
    /// Returns an error when no signal matches and
    /// `FallbackPolicy::Error` or `FallbackPolicy::Probe` is in effect
    /// with nothing on `$PATH`, or when a manifest `onFail = Error`
    /// declaration cannot be satisfied.
    pub(crate) fn resolve_node_pm(&self) -> Result<ResolvedPm, ResolveError> {
        let mut warnings = Vec::new();

        if let Some(o) = self.overrides.pm.as_ref() {
            if !o.pm.can_dispatch_node_scripts() {
                // The user explicitly pinned a PM that can't dispatch
                // package.json scripts. Falling through to step 4-7
                // would silently disregard their intent — surface the
                // mismatch as a hard error instead.
                return Err(ResolveError::InvalidOverride {
                    value: o.pm.label().to_string(),
                    reason: "cannot dispatch package.json scripts (use a Node-ecosystem PM, or \
                            `--pm deno` for Deno tasks)",
                });
            }
            return Ok(ResolvedPm {
                pm: o.pm,
                via: ResolutionStep::Override(o.origin.clone()),
                warnings,
            });
        }
        if let Some(o) = self
            .overrides
            .pm_by_ecosystem
            .get(&Ecosystem::Node)
            .or_else(|| self.overrides.pm_by_ecosystem.get(&Ecosystem::Deno))
        {
            return Ok(ResolvedPm {
                pm: o.pm,
                via: ResolutionStep::Override(o.origin.clone()),
                warnings,
            });
        }

        if let Some(decl) = detect_pm_from_manifest(&self.ctx.root) {
            cross_check_against_lockfile(
                &decl,
                self.ctx,
                self.overrides.on_mismatch,
                &mut warnings,
            )?;
            apply_manifest_on_fail(
                &decl,
                &mut warnings,
                real_binary_check,
                check_version_constraint,
            )?;
            let via = match decl.source {
                ManifestSource::PackageManager => ResolutionStep::ManifestPackageManager,
                ManifestSource::DevEngines => ResolutionStep::ManifestDevEngines {
                    on_fail: decl.on_fail,
                },
            };
            return Ok(ResolvedPm {
                pm: decl.pm,
                via,
                warnings,
            });
        }

        // Filter `primary_pm` through `can_dispatch_node_scripts` so a
        // non-script PM (Cargo/Poetry/Bundler/…) doesn't satisfy the
        // Node lockfile step. Without the filter, a mixed-language repo
        // with `package.json` scripts but only `Cargo.lock` as the
        // top-priority signal would return Cargo here and later bail
        // with the opaque "cargo cannot run scripts" branch instead of
        // continuing to the PATH probe / fallback.
        if let Some(pm) = self.ctx.primary_node_pm().or_else(|| {
            self.ctx
                .primary_pm()
                .filter(|pm| pm.can_dispatch_node_scripts())
        }) {
            return Ok(ResolvedPm {
                pm,
                via: ResolutionStep::Lockfile,
                warnings,
            });
        }

        match self.overrides.fallback {
            FallbackPolicy::Probe => {
                let mut found = probe::probe_all(probe::NODE_PROBE_ORDER);
                if found.is_empty() {
                    return Err(no_pm_found_soft());
                }
                let (picked, binary) = found.remove(0);
                warnings.push(DetectionWarning::PathProbeFallback {
                    picked,
                    ecosystem: Ecosystem::Node,
                    others_available: found.into_iter().map(|(pm, _)| pm).collect(),
                });
                Ok(ResolvedPm {
                    pm: picked,
                    via: ResolutionStep::PathProbe { binary },
                    warnings,
                })
            }
            FallbackPolicy::Npm => {
                warnings.push(DetectionWarning::LegacyNpmFallbackUsed {
                    ecosystem: Ecosystem::Node,
                });
                Ok(ResolvedPm {
                    pm: PackageManager::Npm,
                    via: ResolutionStep::LegacyNpmFallback,
                    warnings,
                })
            }
            FallbackPolicy::Error => Err(no_pm_found_hard()),
        }
    }
}

/// Apply a manifest declaration's `onFail` policy by checking that the
/// declared PM is present on `$PATH` *and*, when a semver range is
/// declared, that the installed version satisfies it.
///
/// - `Ignore` — no check.
/// - `Warn` — emit a `package.json` warning when the PM is missing or
///   the version doesn't match; continue with the declared PM regardless.
/// - `Error` — bail on a missing PM or a version mismatch.
///
/// Version checks that can't run (unparseable range, missing
/// `--version` output, etc.) are skipped silently: the proposal says
/// `onFail` enforces user intent, but blocking dispatch on an
/// unverifiable constraint would be worse than continuing — the binary
/// will surface the real problem at spawn time.
///
/// Binary-presence and version-check side effects are injected so the
/// `Error` branches stay exercisable in unit tests — `Error + missing`
/// and `Error + mismatched version` both `bail!`, which is impossible
/// to cover otherwise without controlling the host `$PATH` and running
/// `<pm> --version` against a real binary. Production callers wire in
/// [`real_binary_check`] and [`check_version_constraint`].
fn apply_manifest_on_fail<P, V>(
    decl: &ManifestPmDecl,
    warnings: &mut Vec<DetectionWarning>,
    is_present: P,
    check_version: V,
) -> Result<(), ResolveError>
where
    P: FnOnce(PackageManager) -> bool,
    V: FnOnce(PackageManager, &str) -> VersionCheck,
{
    if matches!(decl.on_fail, OnFail::Ignore) {
        return Ok(());
    }

    if !is_present(decl.pm) {
        return on_fail_missing_binary(decl, warnings);
    }

    if let Some(range) = decl.version.as_deref()
        && let VersionCheck::Mismatch { declared, actual } = check_version(decl.pm, range)
    {
        return on_fail_version_mismatch(decl, &declared, &actual, warnings);
    }

    Ok(())
}

/// Default binary-presence check used by [`Resolver::resolve_node_pm`].
/// Walks `$PATH` via [`probe::probe`]; injectable in tests so the
/// `Error` branches of [`apply_manifest_on_fail`] are exercisable.
fn real_binary_check(pm: PackageManager) -> bool {
    probe::probe(pm).is_some()
}

fn on_fail_missing_binary(
    decl: &ManifestPmDecl,
    warnings: &mut Vec<DetectionWarning>,
) -> Result<(), ResolveError> {
    match decl.on_fail {
        OnFail::Ignore => Ok(()),
        OnFail::Warn => {
            warnings.push(DetectionWarning::DevEnginesBinaryMissing { pm: decl.pm });
            Ok(())
        }
        OnFail::Error => Err(ResolveError::DevEnginesFailHard {
            pm: decl.pm,
            reason: DevEnginesFailReason::BinaryMissing,
        }),
    }
}

fn on_fail_version_mismatch(
    decl: &ManifestPmDecl,
    declared: &str,
    actual: &str,
    warnings: &mut Vec<DetectionWarning>,
) -> Result<(), ResolveError> {
    match decl.on_fail {
        OnFail::Ignore => Ok(()),
        OnFail::Warn => {
            warnings.push(DetectionWarning::DevEnginesVersionMismatch {
                pm: decl.pm,
                declared: declared.to_string(),
                actual: actual.to_string(),
            });
            Ok(())
        }
        OnFail::Error => Err(ResolveError::DevEnginesFailHard {
            pm: decl.pm,
            reason: DevEnginesFailReason::VersionMismatch {
                declared: declared.to_string(),
                actual: actual.to_string(),
            },
        }),
    }
}

impl ResolvedPm {
    /// Render a one-line description of the chain step that produced this
    /// decision. Used by `--explain` to attribute the PM choice.
    pub(crate) fn describe(&self) -> String {
        match &self.via {
            ResolutionStep::Override(OverrideOrigin::CliFlag) => {
                format!("{} via --pm (CLI override)", self.pm.label())
            }
            ResolutionStep::Override(OverrideOrigin::EnvVar) => {
                format!("{} via RUNNER_PM (environment)", self.pm.label())
            }
            ResolutionStep::Override(OverrideOrigin::ConfigFile { path }) => {
                format!("{} via runner.toml at {}", self.pm.label(), path.display())
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

/// Soft "no PM found" — only emitted from the `Probe` fallback when
/// nothing on `$PATH` matches. Callers that legitimately want to fall
/// through to a direct PATH spawn (`cmd::run::run_pm_exec_fallback`)
/// match on `ResolveError::NoSignalsFound { soft: true, .. }` and swallow
/// it; every other resolver error surfaces to the user.
const fn no_pm_found_soft() -> ResolveError {
    ResolveError::NoSignalsFound {
        ecosystem: Ecosystem::Node,
        soft: true,
    }
}

/// Hard "no PM found" — emitted from `FallbackPolicy::Error`. Carries
/// the same payload but with `soft = false`, so `cmd::run::run`
/// propagates it instead of falling through.
const fn no_pm_found_hard() -> ResolveError {
    ResolveError::NoSignalsFound {
        ecosystem: Ecosystem::Node,
        soft: false,
    }
}

/// Compare a manifest declaration against the lockfile-signal recorded in
/// [`ProjectContext`] and apply the configured [`MismatchPolicy`].
///
/// - [`MismatchPolicy::Warn`] — push a `PmMismatch` warning, declaration wins.
/// - [`MismatchPolicy::Ignore`] — declaration wins silently.
/// - [`MismatchPolicy::Error`] — bail with
///   [`ResolveError::MismatchPolicyError`] so the CLI exits with code 2.
///
/// Manifest declarations frequently come from a project intentionally
/// switching package managers; the new declaration is authoritative, but
/// the stale lockfile is worth flagging so the user can regenerate it.
fn cross_check_against_lockfile(
    decl: &ManifestPmDecl,
    ctx: &ProjectContext,
    policy: MismatchPolicy,
    warnings: &mut Vec<DetectionWarning>,
) -> Result<(), ResolveError> {
    let Some(lockfile_pm) = ctx.primary_node_pm() else {
        return Ok(());
    };
    if lockfile_pm == decl.pm {
        return Ok(());
    }
    let field = match decl.source {
        ManifestSource::PackageManager => "packageManager",
        ManifestSource::DevEngines => "devEngines.packageManager",
    };
    match policy {
        MismatchPolicy::Ignore => Ok(()),
        MismatchPolicy::Warn => {
            warnings.push(DetectionWarning::PmMismatch {
                declared: decl.pm,
                field,
                lockfile: lockfile_pm,
            });
            Ok(())
        }
        MismatchPolicy::Error => Err(ResolveError::MismatchPolicyError {
            declared: decl.pm,
            field,
            lockfile: lockfile_pm,
        }),
    }
}

/// Sources contributing to a [`ResolutionOverrides`].
///
/// Bundles every CLI/env input the resolver consumes so
/// [`ResolutionOverrides::from_sources`] stays extensible —  adding a
/// new override (say `--on-mismatch` / `RUNNER_ON_MISMATCH`) is one
/// field on this struct, not a positional-argument expansion across
/// every test site.
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
/// Production goes through [`ResolutionOverrides::from_cli_and_env`],
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
    /// `--explain` flag presence plus `RUNNER_EXPLAIN` env.
    pub explain: ExplainSource<'a>,
    /// Loaded `runner.toml` if any.
    pub config: Option<&'a LoadedConfig>,
}

/// CLI flag plus env-var value for a string-typed override. The
/// resolver trims and de-duplicates these per the precedence chain in
/// [`parse_override`] (CLI wins; whitespace-only values count as unset).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SourceValue<'a> {
    /// CLI flag value, if the user passed one.
    pub cli: Option<&'a str>,
    /// Env-var value, if set.
    pub env: Option<&'a str>,
}

/// CLI flag (presence) plus env-var value for a boolean-typed override
/// like `--explain` / `RUNNER_EXPLAIN`. CLI wins; env is interpreted by
/// [`is_env_truthy`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ExplainSource<'a> {
    /// `true` when the CLI flag was passed.
    pub cli: bool,
    /// Env-var value, if set.
    pub env: Option<&'a str>,
}

impl ResolutionOverrides {
    /// Assemble overrides from CLI flag values (already parsed by clap),
    /// the `RUNNER_*` environment variables, and an optional `runner.toml`
    /// loaded from the project root.
    ///
    /// Reads `std::env` for the env-var sources; pure parsing happens in
    /// [`Self::from_sources`]. Tests should use `from_sources` directly
    /// with an [`OverrideSources`] builder to inject env values without
    /// touching the process environment.
    ///
    /// # Errors
    ///
    /// Returns an error if any value does not name a known package manager,
    /// task runner, or fallback policy, or if a `runner.toml` field contains
    /// a PM that does not belong to its target ecosystem.
    pub(crate) fn from_cli_and_env(
        cli_pm: Option<&str>,
        cli_runner: Option<&str>,
        cli_fallback: Option<&str>,
        cli_on_mismatch: Option<&str>,
        cli_no_warnings: bool,
        cli_explain: bool,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let env_pm = std::env::var("RUNNER_PM").ok();
        let env_runner = std::env::var("RUNNER_RUNNER").ok();
        let env_fallback = std::env::var("RUNNER_FALLBACK").ok();
        let env_on_mismatch = std::env::var("RUNNER_ON_MISMATCH").ok();
        let env_no_warnings = std::env::var("RUNNER_NO_WARNINGS").ok();
        let env_explain = std::env::var("RUNNER_EXPLAIN").ok();
        Self::from_sources(OverrideSources {
            pm: SourceValue {
                cli: cli_pm,
                env: env_pm.as_deref(),
            },
            runner: SourceValue {
                cli: cli_runner,
                env: env_runner.as_deref(),
            },
            fallback: SourceValue {
                cli: cli_fallback,
                env: env_fallback.as_deref(),
            },
            on_mismatch: SourceValue {
                cli: cli_on_mismatch,
                env: env_on_mismatch.as_deref(),
            },
            no_warnings: ExplainSource {
                cli: cli_no_warnings,
                env: env_no_warnings.as_deref(),
            },
            explain: ExplainSource {
                cli: cli_explain,
                env: env_explain.as_deref(),
            },
            config,
        })
    }

    /// Pure-function constructor that consumes a fully-populated
    /// [`OverrideSources`]. Production code uses
    /// [`Self::from_cli_and_env`], which builds the struct from the
    /// process environment; tests pass values directly so they don't
    /// touch global state.
    ///
    /// # Errors
    ///
    /// Returns an error if any value does not name a known package manager,
    /// task runner, or fallback policy, or if a `runner.toml` field contains
    /// a PM that does not belong to its target ecosystem.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "OverrideSources is a single-use builder; taking by value keeps the call sites moveable"
    )]
    pub(crate) fn from_sources(sources: OverrideSources<'_>) -> Result<Self> {
        let pm = parse_override(
            sources.pm.cli,
            sources.pm.env,
            parse_pm_label,
            |pm, origin| PmOverride { pm, origin },
        )?;
        let runner = parse_override(
            sources.runner.cli,
            sources.runner.env,
            parse_runner_label,
            |runner, origin| RunnerOverride { runner, origin },
        )?;

        let fallback =
            resolve_fallback_policy(sources.fallback.cli, sources.fallback.env, sources.config)?;
        let on_mismatch = resolve_mismatch_policy(
            sources.on_mismatch.cli,
            sources.on_mismatch.env,
            sources.config,
        )?;
        let prefer_runners = parse_prefer_runners(sources.config)?;
        let no_warnings =
            sources.no_warnings.cli || sources.no_warnings.env.is_some_and(is_env_truthy);
        let explain = sources.explain.cli || sources.explain.env.is_some_and(is_env_truthy);

        let mut pm_by_ecosystem = HashMap::new();
        if let Some(loaded) = sources.config {
            if let Some(raw) = loaded.config.pm.node.as_deref() {
                let pm_value = parse_node_pm(raw)?;
                pm_by_ecosystem.insert(
                    pm_value.ecosystem(),
                    PmOverride {
                        pm: pm_value,
                        origin: OverrideOrigin::ConfigFile {
                            path: loaded.path.clone(),
                        },
                    },
                );
            }
            if let Some(raw) = loaded.config.pm.python.as_deref() {
                let pm_value = parse_python_pm(raw)?;
                pm_by_ecosystem.insert(
                    Ecosystem::Python,
                    PmOverride {
                        pm: pm_value,
                        origin: OverrideOrigin::ConfigFile {
                            path: loaded.path.clone(),
                        },
                    },
                );
            }
        }

        Ok(Self {
            pm,
            pm_by_ecosystem,
            runner,
            prefer_runners,
            fallback,
            on_mismatch,
            no_warnings,
            explain,
        })
    }
}

/// Treat any env-var value as truthy unless it's empty, `"0"`, or a
/// case-insensitive variant of `false` / `no` / `off`.
///
/// Surrounding whitespace is stripped first so a trailing newline (the
/// shell-export pattern `RUNNER_EXPLAIN=$VAR \n …`) doesn't accidentally
/// flip an explicit "off" into truthy. Without the case-insensitive
/// compare, `RUNNER_EXPLAIN=FALSE` would silently enable the trace —
/// the opposite of what the user clearly meant.
fn is_env_truthy(raw: &str) -> bool {
    let v = raw.trim();
    !v.is_empty()
        && v != "0"
        && !v.eq_ignore_ascii_case("false")
        && !v.eq_ignore_ascii_case("no")
        && !v.eq_ignore_ascii_case("off")
}

fn parse_fallback_label(raw: &str) -> Result<FallbackPolicy> {
    match raw {
        "probe" => Ok(FallbackPolicy::Probe),
        "npm" => Ok(FallbackPolicy::Npm),
        "error" => Ok(FallbackPolicy::Error),
        other => Err(anyhow!(
            "unknown fallback policy {other:?}; expected one of probe, npm, error",
        )),
    }
}

fn resolve_fallback_policy(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&LoadedConfig>,
) -> Result<FallbackPolicy> {
    // Mirror `parse_override`'s whitespace handling so
    // `RUNNER_FALLBACK=" probe "` and `[resolution].fallback = " npm "`
    // work the same as their stripped forms. Empty/whitespace-only
    // values count as unset and fall through to the next source.
    if let Some(raw) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_fallback_label(raw);
    }
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_fallback_label(raw);
    }
    if let Some(loaded) = config
        && let Some(raw) = loaded
            .config
            .resolution
            .fallback
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return parse_fallback_label(raw);
    }
    Ok(FallbackPolicy::default())
}

/// Parse the `[task_runner].prefer` list, validating each entry against
/// the known [`TaskRunner`] variants. Empty/missing → empty `Vec`.
///
/// Per the resolved design decision, an unknown runner name in the
/// prefer-list is a parse error (not a silent skip) so misconfigured
/// entries surface immediately at startup rather than producing
/// surprising selection results at run time.
fn parse_prefer_runners(config: Option<&LoadedConfig>) -> Result<Vec<TaskRunner>> {
    let Some(loaded) = config else {
        return Ok(Vec::new());
    };
    let raw = &loaded.config.task_runner.prefer;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let runner = TaskRunner::from_label(trimmed).ok_or_else(|| {
            anyhow!(
                "[task_runner].prefer: unknown runner {trimmed:?}; expected one of {}",
                join_labels(TaskRunner::all().iter().map(|r| r.label())),
            )
        })?;
        out.push(runner);
    }
    Ok(out)
}

fn parse_mismatch_label(raw: &str) -> Result<MismatchPolicy> {
    match raw {
        "warn" => Ok(MismatchPolicy::Warn),
        "error" => Ok(MismatchPolicy::Error),
        "ignore" => Ok(MismatchPolicy::Ignore),
        other => Err(anyhow!(
            "unknown on-mismatch policy {other:?}; expected one of warn, error, ignore",
        )),
    }
}

fn resolve_mismatch_policy(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&LoadedConfig>,
) -> Result<MismatchPolicy> {
    if let Some(raw) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_mismatch_label(raw);
    }
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        return parse_mismatch_label(raw);
    }
    if let Some(loaded) = config
        && let Some(raw) = loaded
            .config
            .resolution
            .on_mismatch
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return parse_mismatch_label(raw);
    }
    Ok(MismatchPolicy::default())
}

fn parse_pm_label(raw: &str) -> Result<PackageManager> {
    PackageManager::from_label(raw).ok_or_else(|| {
        anyhow!(
            "unknown package manager {raw:?}; expected one of {}",
            join_labels(
                PackageManager::all()
                    .iter()
                    .copied()
                    .map(PackageManager::label)
            ),
        )
    })
}

fn parse_runner_label(raw: &str) -> Result<TaskRunner> {
    TaskRunner::from_label(raw).ok_or_else(|| {
        anyhow!(
            "unknown task runner {raw:?}; expected one of {}",
            join_labels(TaskRunner::all().iter().copied().map(TaskRunner::label)),
        )
    })
}

/// Generic CLI-then-env override parser. CLI wins; whitespace is
/// trimmed from both sources before parsing so `RUNNER_PM=" pnpm "`
/// works the same as `RUNNER_PM=pnpm`. Empty/whitespace-only values
/// are treated as unset so a user can clear an inherited variable with
/// `RUNNER_PM= runner …`. Matches the whitespace handling used by
/// [`is_env_truthy`] for boolean env flags.
fn parse_override<T, P, V, B>(
    cli: Option<&str>,
    env: Option<&str>,
    parse: V,
    build: B,
) -> Result<Option<T>>
where
    V: Fn(&str) -> Result<P>,
    B: Fn(P, OverrideOrigin) -> T,
{
    if let Some(raw) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = parse(raw)?;
        return Ok(Some(build(parsed, OverrideOrigin::CliFlag)));
    }
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = parse(raw)?;
        return Ok(Some(build(parsed, OverrideOrigin::EnvVar)));
    }
    Ok(None)
}

fn join_labels<I: Iterator<Item = &'static str>>(labels: I) -> String {
    labels.collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::{
        ExplainSource, FallbackPolicy, OverrideOrigin, OverrideSources, PmOverride,
        ResolutionOverrides, ResolutionStep, ResolveError, Resolver, RunnerOverride, SourceValue,
    };
    use crate::config::{LoadedConfig, PmSection, RunnerConfig};
    use crate::types::{Ecosystem, PackageManager, ProjectContext, TaskRunner};

    fn context(package_managers: Vec<PackageManager>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers,
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn resolver<'ctx>(
        ctx: &'ctx ProjectContext,
        overrides: &'ctx ResolutionOverrides,
    ) -> Resolver<'ctx> {
        Resolver::new(ctx, overrides)
    }

    fn with_pm_override(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..ResolutionOverrides::default()
        }
    }

    fn with_config_pm(pm: PackageManager, eco: Ecosystem) -> ResolutionOverrides {
        let mut map = HashMap::new();
        map.insert(
            eco,
            PmOverride {
                pm,
                origin: OverrideOrigin::ConfigFile {
                    path: PathBuf::from("/test/runner.toml"),
                },
            },
        );
        ResolutionOverrides {
            pm_by_ecosystem: map,
            ..ResolutionOverrides::default()
        }
    }

    #[test]
    fn resolves_detected_node_pm_via_lockfile() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = ResolutionOverrides::default();
        let decision = resolver(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_legacy_npm_when_fallback_policy_is_npm() {
        let ctx = context(vec![]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Npm,
            ..ResolutionOverrides::default()
        };
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("legacy npm fallback should succeed");

        assert_eq!(decision.pm, PackageManager::Npm);
        assert_eq!(decision.via, ResolutionStep::LegacyNpmFallback);
    }

    #[test]
    fn fallback_error_policy_returns_helpful_error_when_no_signal() {
        let ctx = context(vec![]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Error,
            ..ResolutionOverrides::default()
        };
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail when nothing matches");

        let msg = format!("{err}");
        assert!(msg.contains("no node package manager detected"));
        assert!(msg.contains("--pm"));
    }

    #[test]
    fn fallback_error_policy_is_a_hard_no_signals_found() {
        // `--fallback=error` is the user opting into strict mode. The
        // error must propagate through `cmd::run::run` instead of
        // collapsing into the soft fall-through, so it carries
        // `soft: false`.
        let ctx = context(vec![]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Error,
            ..ResolutionOverrides::default()
        };
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail");

        assert!(
            matches!(err, ResolveError::NoSignalsFound { soft: false, .. }),
            "error-policy failure must be hard, got: {err:?}"
        );
    }

    #[test]
    fn fallback_probe_with_empty_path_yields_soft_no_signals_found() {
        // The soft case: nothing declared, nothing on PATH. The
        // arbitrary-command fallback in `cmd::run::run` legitimately
        // wants to drop this and try a direct PATH spawn, so the error
        // carries `soft: true`.
        let ctx = context(vec![]);
        let overrides = ResolutionOverrides::default();
        // The probe call is a real `which`-style lookup; if a bun/pnpm
        // binary happens to be installed on the test host the test
        // becomes a no-op rather than a false failure.
        let result = Resolver::new(&ctx, &overrides).resolve_node_pm();
        match result {
            Ok(_) => {
                // PATH probe found a real PM on the host. Test
                // becomes a non-assertion; the soft-sentinel branch
                // is exercised by other resolver tests that mock
                // the probe directly.
            }
            Err(e) => assert!(
                matches!(e, ResolveError::NoSignalsFound { soft: true, .. }),
                "probe-policy miss must be the soft variant, got: {e:?}"
            ),
        }
    }

    #[test]
    fn prefers_node_pm_over_non_node_primary() {
        let ctx = context(vec![PackageManager::Cargo, PackageManager::Bun]);
        let overrides = ResolutionOverrides::default();
        let decision = resolver(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_primary_pm_when_no_node_pm_detected() {
        let ctx = context(vec![PackageManager::Deno]);
        let overrides = ResolutionOverrides::default();
        let decision = resolver(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Deno);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn cli_override_beats_detected_pm() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Yarn, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(
            decision.via,
            ResolutionStep::Override(OverrideOrigin::CliFlag)
        );
    }

    #[test]
    fn env_override_beats_detected_pm() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Bun, OverrideOrigin::EnvVar);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(
            decision.via,
            ResolutionStep::Override(OverrideOrigin::EnvVar)
        );
    }

    #[test]
    fn pm_override_for_deno_is_honored_by_node_resolver() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Deno, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn cross_ecosystem_pm_override_for_node_scripts_is_a_hard_error() {
        // Pre-B5 behavior silently dropped the override and fell
        // through to the lockfile signal. That hid user intent — an
        // explicit `--pm cargo` on a Node project is almost always a
        // misconfiguration the user wants surfaced, not silently
        // disregarded. Now `InvalidOverride` is returned so `main`
        // exits 2 with a clear message.
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Cargo, OverrideOrigin::CliFlag);
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("cargo cannot dispatch package.json scripts");

        assert!(
            matches!(err, ResolveError::InvalidOverride { ref value, .. } if value == "cargo"),
            "expected InvalidOverride for cargo, got: {err:?}",
        );
    }

    #[test]
    fn cli_pm_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("--pm yarn should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
        assert!(overrides.runner.is_none());
    }

    #[test]
    fn env_pm_value_parses_when_cli_absent() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("bun"),
            },
            ..OverrideSources::default()
        })
        .expect("RUNNER_PM=bun should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Bun);
        assert_eq!(pm.origin, OverrideOrigin::EnvVar);
    }

    #[test]
    fn cli_wins_over_env() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: Some("bun"),
            },
            ..OverrideSources::default()
        })
        .expect("both sources should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn empty_env_is_treated_as_unset() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some(""),
            },
            ..OverrideSources::default()
        })
        .expect("empty env should parse as no override");

        assert!(overrides.pm.is_none());
    }

    #[test]
    fn cli_runner_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            runner: SourceValue {
                cli: Some("just"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("--runner just should parse");

        let runner: RunnerOverride = overrides.runner.expect("runner override should be present");
        assert_eq!(runner.runner, TaskRunner::Just);
        assert_eq!(runner.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn unknown_pm_label_errors_with_valid_value_list() {
        let err = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: Some("zoot"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown PM should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown package manager"));
        assert!(msg.contains("npm"));
        assert!(msg.contains("pnpm"));
    }

    #[test]
    fn unknown_runner_label_errors_with_valid_value_list() {
        let err = ResolutionOverrides::from_sources(OverrideSources {
            runner: SourceValue {
                cli: Some("zoot"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown runner should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown task runner"));
        assert!(msg.contains("turbo"));
    }

    #[test]
    fn bundler_alias_bundle_is_accepted() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: Some("bundle"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("`bundle` should alias to bundler");

        assert_eq!(
            overrides.pm.expect("pm should be present").pm,
            PackageManager::Bundler,
        );
    }

    #[test]
    fn go_task_alias_is_accepted() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            runner: SourceValue {
                cli: Some("go-task"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("`go-task` should alias to GoTask");

        assert_eq!(
            overrides.runner.expect("runner should be present").runner,
            TaskRunner::GoTask,
        );
    }

    fn loaded_config_with_node(node: &str) -> LoadedConfig {
        LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            config: RunnerConfig {
                pm: PmSection {
                    node: Some(node.to_owned()),
                    python: None,
                },
                ..RunnerConfig::default()
            },
        }
    }

    #[test]
    fn config_pm_node_field_overrides_detection() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_config_pm(PackageManager::Yarn, Ecosystem::Node);

        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Yarn);
        match decision.via {
            ResolutionStep::Override(OverrideOrigin::ConfigFile { .. }) => {}
            other => panic!("expected Override(ConfigFile), got {other:?}"),
        }
    }

    #[test]
    fn cli_override_beats_config_override() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let mut overrides = with_config_pm(PackageManager::Yarn, Ecosystem::Node);
        overrides.pm = Some(PmOverride {
            pm: PackageManager::Bun,
            origin: OverrideOrigin::CliFlag,
        });

        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        match decision.via {
            ResolutionStep::Override(OverrideOrigin::CliFlag) => {}
            other => panic!("expected CliFlag origin, got {other:?}"),
        }
    }

    #[test]
    fn config_loaded_value_populates_pm_by_ecosystem() {
        let loaded = loaded_config_with_node("bun");
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("config-only overrides should parse");

        assert!(overrides.pm.is_none());
        let entry = overrides
            .pm_by_ecosystem
            .get(&Ecosystem::Node)
            .expect("Node ecosystem entry should be present");
        assert_eq!(entry.pm, PackageManager::Bun);
        match &entry.origin {
            OverrideOrigin::ConfigFile { path } => {
                assert!(path.ends_with("runner.toml"));
            }
            other => panic!("expected ConfigFile origin, got {other:?}"),
        }
    }

    #[test]
    fn config_python_pm_keyed_under_python_ecosystem() {
        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            config: RunnerConfig {
                pm: PmSection {
                    node: None,
                    python: Some("uv".to_owned()),
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("python config should parse");

        let entry = overrides
            .pm_by_ecosystem
            .get(&Ecosystem::Python)
            .expect("python ecosystem entry should be present");
        assert_eq!(entry.pm, PackageManager::Uv);
    }

    #[test]
    fn config_cross_ecosystem_node_value_rejected_at_parse_time() {
        let loaded = loaded_config_with_node("cargo");
        let err = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("cargo is not a node-script PM");
        assert!(format!("{err}").contains("cannot dispatch package.json scripts"));
    }

    #[test]
    fn manifest_package_manager_field_beats_lockfile_signal() {
        use std::fs;

        use crate::detect::detect;
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("resolver-manifest-wins");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4.3.0" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("lockfile should be written");

        let ctx = detect(dir.path());
        // Detection picks the lockfile signal as primary; the resolver
        // should override that to honor the manifest declaration.
        assert!(ctx.package_managers.contains(&PackageManager::Pnpm));

        let decision = Resolver::new(&ctx, &ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(decision.via, ResolutionStep::ManifestPackageManager);
        assert_eq!(decision.warnings.len(), 1);
        assert_eq!(decision.warnings[0].source(), "package.json");
        let detail = decision.warnings[0].detail();
        assert!(
            detail.contains("declaration wins"),
            "warning should mention declaration wins: {detail}",
        );
    }

    #[test]
    fn dev_engines_used_when_package_manager_absent() {
        use std::fs;

        use crate::detect::detect;
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("resolver-dev-engines-only");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "bun", "onFail": "warn" } } }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());
        let decision = Resolver::new(&ctx, &ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        match decision.via {
            ResolutionStep::ManifestDevEngines { .. } => {}
            other => panic!("expected ManifestDevEngines, got {other:?}"),
        }
    }

    #[test]
    fn cli_override_still_beats_manifest_declaration() {
        use std::fs;

        use crate::detect::detect;
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("resolver-cli-beats-manifest");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4" }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());
        let overrides = with_pm_override(PackageManager::Bun, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(
            decision.via,
            ResolutionStep::Override(OverrideOrigin::CliFlag)
        );
    }

    #[test]
    fn matching_lockfile_and_manifest_produce_no_warning() {
        use std::fs;

        use crate::detect::detect;
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("resolver-matching-signals");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "pnpm@9" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("lockfile should be written");

        let ctx = detect(dir.path());
        let decision = Resolver::new(&ctx, &ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::ManifestPackageManager);
        assert!(decision.warnings.is_empty());
    }

    fn mismatch_dir(name: &str) -> crate::tool::test_support::TempDir {
        use std::fs;

        use crate::tool::test_support::TempDir;
        let dir = TempDir::new(name);
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("pnpm-lock.yaml should be written");
        dir
    }

    #[test]
    fn on_mismatch_warn_emits_warning_and_keeps_declaration() {
        use super::MismatchPolicy;
        use crate::detect::detect;

        let dir = mismatch_dir("mismatch-warn");
        let ctx = detect(dir.path());
        let overrides = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Warn,
            ..ResolutionOverrides::default()
        };
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("warn policy should not bail");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(decision.warnings.len(), 1);
        assert!(decision.warnings[0].detail().contains("declaration wins"));
    }

    #[test]
    fn on_mismatch_ignore_silently_keeps_declaration() {
        use super::MismatchPolicy;
        use crate::detect::detect;

        let dir = mismatch_dir("mismatch-ignore");
        let ctx = detect(dir.path());
        let overrides = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Ignore,
            ..ResolutionOverrides::default()
        };
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("ignore policy should not bail");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert!(decision.warnings.is_empty());
    }

    #[test]
    fn on_mismatch_error_bails_with_resolve_error() {
        use super::MismatchPolicy;
        use crate::detect::detect;

        let dir = mismatch_dir("mismatch-error");
        let ctx = detect(dir.path());
        let overrides = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Error,
            ..ResolutionOverrides::default()
        };
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail on mismatch");

        assert!(
            matches!(err, ResolveError::MismatchPolicyError { .. }),
            "expected MismatchPolicyError, got: {err:?}"
        );
    }

    #[test]
    fn prefer_runners_parses_known_labels() {
        use crate::config::{LoadedConfig, RunnerConfig, TaskRunnerSection};

        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            config: RunnerConfig {
                task_runner: TaskRunnerSection {
                    prefer: vec!["just".to_string(), "turbo".to_string()],
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("prefer list of known runners should parse");

        assert_eq!(
            overrides.prefer_runners,
            vec![TaskRunner::Just, TaskRunner::Turbo],
        );
    }

    #[test]
    fn prefer_runners_rejects_unknown_label() {
        use crate::config::{LoadedConfig, RunnerConfig, TaskRunnerSection};

        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            config: RunnerConfig {
                task_runner: TaskRunnerSection {
                    prefer: vec!["zoot".to_string()],
                },
                ..RunnerConfig::default()
            },
        };
        let err = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("unknown runner label must error at parse time");

        let msg = format!("{err}");
        assert!(msg.contains("unknown runner"), "got: {msg}");
        assert!(msg.contains("zoot"), "got: {msg}");
    }

    #[test]
    fn on_mismatch_label_parses_three_values() {
        use super::{MismatchPolicy, parse_mismatch_label};

        assert_eq!(parse_mismatch_label("warn").unwrap(), MismatchPolicy::Warn);
        assert_eq!(
            parse_mismatch_label("error").unwrap(),
            MismatchPolicy::Error
        );
        assert_eq!(
            parse_mismatch_label("ignore").unwrap(),
            MismatchPolicy::Ignore
        );
        assert!(parse_mismatch_label("nope").is_err());
    }

    #[test]
    fn manifest_on_fail_error_bails_when_binary_missing() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        // onFail=Error + the declared PM is missing from PATH should
        // surface as a fatal error so the user knows their pinned
        // toolchain expectation can't be met. Tested with injected
        // checkers since the real PATH is unpredictable in CI.
        let decl = ManifestPmDecl {
            pm: PackageManager::Yarn,
            source: ManifestSource::DevEngines,
            version: None,
            on_fail: OnFail::Error,
        };
        let mut warnings = Vec::new();

        let err = super::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| false,
            |_, _| VersionCheck::Unverifiable {
                reason: String::new(),
            },
        )
        .expect_err("Error + missing should bail");

        let msg = format!("{err}");
        assert!(msg.contains("yarn"), "error should name the PM: {msg}");
        assert!(
            msg.contains("not found on PATH"),
            "error should explain: {msg}"
        );
        assert!(
            msg.contains("onFail=error"),
            "error should attribute: {msg}"
        );
    }

    #[test]
    fn manifest_on_fail_error_bails_when_version_mismatched() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        // onFail=Error + version range that doesn't match what's
        // installed → fatal. Tests the path that requires both PATH
        // success and a Mismatch result from the version checker.
        let decl = ManifestPmDecl {
            pm: PackageManager::Pnpm,
            source: ManifestSource::DevEngines,
            version: Some(">=9.0.0".to_string()),
            on_fail: OnFail::Error,
        };
        let mut warnings = Vec::new();

        let err = super::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| true,
            |_, _| VersionCheck::Mismatch {
                declared: ">=9.0.0".to_string(),
                actual: "8.15.0".to_string(),
            },
        )
        .expect_err("Error + version mismatch should bail");

        let msg = format!("{err}");
        assert!(msg.contains("pnpm"));
        assert!(msg.contains(">=9.0.0"));
        assert!(msg.contains("8.15.0"));
        assert!(msg.contains("onFail=error"));
    }

    #[test]
    fn manifest_on_fail_warn_emits_warning_when_binary_missing() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        let decl = ManifestPmDecl {
            pm: PackageManager::Bun,
            source: ManifestSource::DevEngines,
            version: None,
            on_fail: OnFail::Warn,
        };
        let mut warnings = Vec::new();

        super::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| false,
            |_, _| VersionCheck::Unverifiable {
                reason: String::new(),
            },
        )
        .expect("Warn should not bail");

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].source(), "package.json");
        assert!(warnings[0].detail().contains("bun"));
    }

    #[test]
    fn manifest_on_fail_warn_emits_warning_on_version_mismatch() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        let decl = ManifestPmDecl {
            pm: PackageManager::Yarn,
            source: ManifestSource::DevEngines,
            version: Some("^4.0.0".to_string()),
            on_fail: OnFail::Warn,
        };
        let mut warnings = Vec::new();

        super::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| true,
            |_, _| VersionCheck::Mismatch {
                declared: "^4.0.0".to_string(),
                actual: "1.22.22".to_string(),
            },
        )
        .expect("Warn should not bail");

        assert_eq!(warnings.len(), 1);
        let detail = warnings[0].detail();
        assert!(detail.contains("yarn"));
        assert!(detail.contains("^4.0.0"));
        assert!(detail.contains("1.22.22"));
    }

    #[test]
    fn manifest_on_fail_ignore_skips_all_checks() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail};

        // OnFail=Ignore short-circuits before the binary/version checks
        // even run — they should never be called. Use a panicking
        // checker to prove the early return holds.
        let decl = ManifestPmDecl {
            pm: PackageManager::Npm,
            source: ManifestSource::DevEngines,
            version: Some(">=20".to_string()),
            on_fail: OnFail::Ignore,
        };
        let mut warnings = Vec::new();

        super::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| panic!("presence check should not run when onFail=Ignore"),
            |_, _| panic!("version check should not run when onFail=Ignore"),
        )
        .expect("Ignore should always succeed");

        assert!(warnings.is_empty());
    }

    #[test]
    fn manifest_on_fail_unverifiable_version_continues_without_warning() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        // Version checks that can't run (unparseable range, missing
        // --version output) collapse to Unverifiable — that path must
        // continue silently, not warn or bail, otherwise a partially
        // broken environment blocks dispatch unnecessarily.
        let decl = ManifestPmDecl {
            pm: PackageManager::Yarn,
            source: ManifestSource::DevEngines,
            version: Some("not-a-valid-range".to_string()),
            on_fail: OnFail::Error,
        };
        let mut warnings = Vec::new();

        super::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| true,
            |_, _| VersionCheck::Unverifiable {
                reason: "unparseable range".to_string(),
            },
        )
        .expect("Unverifiable should continue, not bail");

        assert!(warnings.is_empty());
    }

    #[test]
    fn from_sources_builder_is_ergonomic_for_partial_overrides() {
        // Demonstrates the canonical idiom: construct only the fields
        // that matter, default the rest. All sibling tests in this module
        // use the same shape.
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: None,
            },
            explain: ExplainSource {
                cli: true,
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("structured override should parse");

        assert_eq!(
            overrides.pm.expect("pm override should be present").pm,
            PackageManager::Yarn
        );
        assert!(overrides.explain);
        assert!(overrides.runner.is_none());
    }

    #[test]
    fn parse_override_trims_whitespace_in_env_and_cli() {
        // Whitespace in env values is common when shell-export patterns
        // leave trailing newlines or quoted values pad arguments. The
        // override parser must tolerate this so `RUNNER_PM=" pnpm "`
        // works the same as `RUNNER_PM=pnpm` instead of erroring on an
        // "unknown package manager" with the padded label.
        let from_env = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some(" pnpm "),
            },
            ..OverrideSources::default()
        })
        .expect("padded env value should parse after trimming");
        assert_eq!(
            from_env.pm.expect("pm should be present").pm,
            PackageManager::Pnpm
        );

        let from_cli = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: Some(" yarn\n"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("padded CLI value should parse after trimming");
        assert_eq!(
            from_cli.pm.expect("pm should be present").pm,
            PackageManager::Yarn
        );

        // Whitespace-only values are treated as unset (same as empty
        // strings); without this, `RUNNER_PM="   "` would fail with
        // "unknown package manager \"\"" after the trim.
        let blank = ResolutionOverrides::from_sources(OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("   "),
            },
            ..OverrideSources::default()
        })
        .expect("whitespace-only env should parse as no override");
        assert!(blank.pm.is_none());
    }

    #[test]
    fn is_env_truthy_is_case_insensitive_for_falsy_values() {
        use super::is_env_truthy;

        // Falsy values in any case should be falsy.
        assert!(!is_env_truthy("false"));
        assert!(!is_env_truthy("FALSE"));
        assert!(!is_env_truthy("False"));
        assert!(!is_env_truthy("no"));
        assert!(!is_env_truthy("NO"));
        assert!(!is_env_truthy("off"));
        assert!(!is_env_truthy("OFF"));
        assert!(!is_env_truthy("Off"));
        assert!(!is_env_truthy("0"));
        assert!(!is_env_truthy(""));

        // Surrounding whitespace shouldn't flip a falsy value.
        assert!(!is_env_truthy("  false  "));
        assert!(!is_env_truthy("\nfalse\n"));

        // Anything else is truthy.
        assert!(is_env_truthy("1"));
        assert!(is_env_truthy("true"));
        assert!(is_env_truthy("yes"));
        assert!(is_env_truthy("on"));
        assert!(is_env_truthy("anything"));
    }

    #[test]
    fn describe_renders_human_friendly_step_label() {
        use crate::tool::node::OnFail;

        // Table-driven: each row pairs a decision with the exact string
        // it must produce. Locks down the provenance wording that
        // `--explain` and `runner why` surface verbatim — a casual
        // re-phrase shouldn't slip through silently.
        let cases: &[(super::ResolvedPm, &str)] = &[
            (
                super::ResolvedPm {
                    pm: PackageManager::Yarn,
                    via: ResolutionStep::Override(OverrideOrigin::CliFlag),
                    warnings: vec![],
                },
                "yarn via --pm (CLI override)",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Bun,
                    via: ResolutionStep::Override(OverrideOrigin::EnvVar),
                    warnings: vec![],
                },
                "bun via RUNNER_PM (environment)",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Pnpm,
                    via: ResolutionStep::Override(OverrideOrigin::ConfigFile {
                        path: PathBuf::from("/proj/runner.toml"),
                    }),
                    warnings: vec![],
                },
                "pnpm via runner.toml at /proj/runner.toml",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Pnpm,
                    via: ResolutionStep::ManifestPackageManager,
                    warnings: vec![],
                },
                "pnpm via package.json \"packageManager\"",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Bun,
                    via: ResolutionStep::ManifestDevEngines {
                        on_fail: OnFail::Error,
                    },
                    warnings: vec![],
                },
                "bun via package.json \"devEngines.packageManager\" (onFail=Error)",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Pnpm,
                    via: ResolutionStep::Lockfile,
                    warnings: vec![],
                },
                "pnpm via detected lockfile",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Npm,
                    via: ResolutionStep::PathProbe {
                        binary: PathBuf::from("/usr/bin/npm"),
                    },
                    warnings: vec![],
                },
                "npm via PATH probe at /usr/bin/npm",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Npm,
                    via: ResolutionStep::LegacyNpmFallback,
                    warnings: vec![],
                },
                "npm via --fallback=npm (legacy)",
            ),
        ];

        for (decision, expected) in cases {
            assert_eq!(&decision.describe(), expected);
        }
    }

    #[test]
    fn deno_config_value_lands_under_deno_ecosystem_and_resolves_for_node_scripts() {
        // The runner.toml field is `[pm].node = "deno"`; the resolver
        // stores it under Ecosystem::Deno (per PackageManager::ecosystem)
        // and the Node-script resolver consults both Node and Deno keys.
        let loaded = loaded_config_with_node("deno");
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("deno config should parse");

        assert!(overrides.pm_by_ecosystem.contains_key(&Ecosystem::Deno));

        let ctx = context(vec![PackageManager::Pnpm]);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");
        assert_eq!(decision.pm, PackageManager::Deno);
    }
}
