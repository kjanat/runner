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

mod probe;

/// Re-export of the pure-function probe variant for the `doctor`
/// subcommand. Lets `cmd::doctor` exercise the same PATH walk the
/// resolver uses without owning the env-reading logic.
pub(crate) use probe::probe_in as probe_path_for_doctor;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

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
    overrides: ResolutionOverrides,
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
    /// Task-runner override. Parsed and stored in Phase 2; consumed by the
    /// source-selection chain generalized in Phase 8.
    #[allow(dead_code, reason = "consumed by source selection in Phase 8")]
    pub runner: Option<RunnerOverride>,
    /// What to do when no signal in steps 2–6 matches.
    pub fallback: FallbackPolicy,
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
    #[allow(dead_code, reason = "consumed by source selection in Phase 8")]
    pub runner: TaskRunner,
    /// Where the override came from.
    #[allow(dead_code, reason = "consumed by --explain in Phase 6")]
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
        /// Absolute path the override was loaded from.
        #[allow(dead_code, reason = "consumed by --explain in Phase 6")]
        path: PathBuf,
    },
}

/// A package-manager decision plus the chain step that produced it.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPm {
    /// The chosen package manager.
    pub pm: PackageManager,
    /// Which step of the resolution chain produced [`Self::pm`]. Consumed by
    /// the diagnostic surface added in a later phase (`--explain` /
    /// `runner why`); kept on the result type now so resolver internals can
    /// already populate it without churning the public API later.
    #[allow(dead_code, reason = "consumed by --explain in a later phase")]
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
        /// Effective `onFail` value for the chosen entry.
        #[allow(dead_code, reason = "consumed by --explain in Phase 6")]
        on_fail: OnFail,
    },
    /// Step 6 — package manager inferred from a lockfile (or another
    /// detector recorded in [`ProjectContext::package_managers`]).
    Lockfile,
    /// Step 7 — discovered via `$PATH` probe in canonical order.
    PathProbe {
        /// Absolute path of the executable found on PATH.
        #[allow(dead_code, reason = "consumed by --explain in Phase 6")]
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
    pub(crate) const fn new(ctx: &'ctx ProjectContext, overrides: ResolutionOverrides) -> Self {
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
    pub(crate) fn resolve_node_pm(&self) -> Result<ResolvedPm> {
        let mut warnings = Vec::new();

        if let Some(o) = self.overrides.pm.as_ref()
            && pm_can_run_package_json_scripts(o.pm)
        {
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
            if let Some(warning) = cross_check_against_lockfile(&decl, self.ctx) {
                warnings.push(warning);
            }
            apply_manifest_on_fail(&decl, &mut warnings)?;
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

        if let Some(pm) = self.ctx.primary_node_pm().or_else(|| self.ctx.primary_pm()) {
            return Ok(ResolvedPm {
                pm,
                via: ResolutionStep::Lockfile,
                warnings,
            });
        }

        match self.overrides.fallback {
            FallbackPolicy::Probe => match probe::probe_first(probe::NODE_PROBE_ORDER) {
                Some((pm, binary)) => Ok(ResolvedPm {
                    pm,
                    via: ResolutionStep::PathProbe { binary },
                    warnings,
                }),
                None => Err(no_pm_found_error()),
            },
            FallbackPolicy::Npm => Ok(ResolvedPm {
                pm: PackageManager::Npm,
                via: ResolutionStep::LegacyNpmFallback,
                warnings,
            }),
            FallbackPolicy::Error => Err(no_pm_found_error()),
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
fn apply_manifest_on_fail(
    decl: &ManifestPmDecl,
    warnings: &mut Vec<DetectionWarning>,
) -> Result<()> {
    if matches!(decl.on_fail, OnFail::Ignore) {
        return Ok(());
    }

    if probe::probe(decl.pm.label()).is_none() {
        return on_fail_missing_binary(decl, warnings);
    }

    if let Some(range) = decl.version.as_deref()
        && let VersionCheck::Mismatch { declared, actual } =
            check_version_constraint(decl.pm, range)
    {
        return on_fail_version_mismatch(decl, &declared, &actual, warnings);
    }

    Ok(())
}

fn on_fail_missing_binary(
    decl: &ManifestPmDecl,
    warnings: &mut Vec<DetectionWarning>,
) -> Result<()> {
    let detail = format!(
        "devEngines.packageManager declares {} but it was not found on PATH",
        decl.pm.label(),
    );
    match decl.on_fail {
        OnFail::Ignore => Ok(()),
        OnFail::Warn => {
            warnings.push(DetectionWarning {
                source: "package.json",
                detail: format!("{detail}; dispatch will fail at spawn time"),
            });
            Ok(())
        }
        OnFail::Error => bail!("{detail} (onFail=error)"),
    }
}

fn on_fail_version_mismatch(
    decl: &ManifestPmDecl,
    declared: &str,
    actual: &str,
    warnings: &mut Vec<DetectionWarning>,
) -> Result<()> {
    let detail = format!(
        "devEngines.packageManager requires {} {declared} but the installed version is {actual}",
        decl.pm.label(),
    );
    match decl.on_fail {
        OnFail::Ignore => Ok(()),
        OnFail::Warn => {
            warnings.push(DetectionWarning {
                source: "package.json",
                detail,
            });
            Ok(())
        }
        OnFail::Error => bail!("{detail} (onFail=error)"),
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

fn no_pm_found_error() -> anyhow::Error {
    anyhow!(
        "no Node package manager detected. Checked: lockfiles (bun.lock, pnpm-lock.yaml, \
         yarn.lock, package-lock.json), package.json (packageManager + devEngines), PATH for \
         bun/pnpm/yarn/npm. Pin one with `--pm <name>`, set `RUNNER_PM=<name>`, add it to \
         runner.toml under `[pm].node`, or install one of {{bun, pnpm, yarn, npm}}.",
    )
}

/// Compare a manifest declaration against the lockfile-signal recorded in
/// [`ProjectContext`]. Returns a `package.json` warning when the two
/// disagree; `None` when they match or when no lockfile signal exists.
///
/// Manifest declarations frequently come from a project intentionally
/// switching package managers; the new declaration is authoritative, but
/// the stale lockfile is worth flagging so the user can regenerate it.
fn cross_check_against_lockfile(
    decl: &ManifestPmDecl,
    ctx: &ProjectContext,
) -> Option<DetectionWarning> {
    let lockfile_pm = ctx.primary_node_pm()?;
    if lockfile_pm == decl.pm {
        return None;
    }
    let field = match decl.source {
        ManifestSource::PackageManager => "packageManager",
        ManifestSource::DevEngines => "devEngines.packageManager",
    };
    Some(DetectionWarning {
        source: "package.json",
        detail: format!(
            "{field} declares {} but the lockfile reflects {} — declaration wins; regenerate the \
             lockfile to silence this",
            decl.pm.label(),
            lockfile_pm.label(),
        ),
    })
}

const fn pm_can_run_package_json_scripts(pm: PackageManager) -> bool {
    pm.is_node() || matches!(pm, PackageManager::Deno)
}

impl ResolutionOverrides {
    /// Assemble overrides from CLI flag values (already parsed by clap),
    /// the `RUNNER_*` environment variables, and an optional `runner.toml`
    /// loaded from the project root.
    ///
    /// Reads `std::env` for the env-var sources; pure parsing happens in
    /// [`Self::from_values`].
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
        cli_explain: bool,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let env_pm = std::env::var("RUNNER_PM").ok();
        let env_runner = std::env::var("RUNNER_RUNNER").ok();
        let env_fallback = std::env::var("RUNNER_FALLBACK").ok();
        let env_explain = std::env::var("RUNNER_EXPLAIN").ok();
        Self::from_values_with_explain(
            cli_pm,
            env_pm.as_deref(),
            cli_runner,
            env_runner.as_deref(),
            cli_fallback,
            env_fallback.as_deref(),
            cli_explain,
            env_explain.as_deref(),
            config,
        )
    }

    /// Pure-function variant of [`Self::from_cli_and_env`] without the
    /// `--explain` toggle (always disabled). Tests use this directly;
    /// production code goes through [`Self::from_cli_and_env`].
    ///
    /// # Errors
    ///
    /// Returns an error if any value does not name a known package manager,
    /// task runner, or fallback policy, or if a `runner.toml` field contains
    /// a PM that does not belong to its target ecosystem.
    #[cfg(test)]
    pub(crate) fn from_values(
        cli_pm: Option<&str>,
        env_pm: Option<&str>,
        cli_runner: Option<&str>,
        env_runner: Option<&str>,
        cli_fallback: Option<&str>,
        env_fallback: Option<&str>,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        Self::from_values_with_explain(
            cli_pm,
            env_pm,
            cli_runner,
            env_runner,
            cli_fallback,
            env_fallback,
            false,
            None,
            config,
        )
    }

    /// Pure-function variant that also accepts the `--explain` /
    /// `RUNNER_EXPLAIN` toggle. CLI value (`Some(true)` when the flag is
    /// passed) wins over env (any non-empty value is truthy).
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::from_values`].
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit source injection for testability across all six override sources"
    )]
    pub(crate) fn from_values_with_explain(
        cli_pm: Option<&str>,
        env_pm: Option<&str>,
        cli_runner: Option<&str>,
        env_runner: Option<&str>,
        cli_fallback: Option<&str>,
        env_fallback: Option<&str>,
        cli_explain: bool,
        env_explain: Option<&str>,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let pm = parse_override(cli_pm, env_pm, parse_pm_label, |pm, origin| PmOverride {
            pm,
            origin,
        })?;
        let runner = parse_override(
            cli_runner,
            env_runner,
            parse_runner_label,
            |runner, origin| RunnerOverride { runner, origin },
        )?;

        let fallback = resolve_fallback_policy(cli_fallback, env_fallback, config)?;
        let explain = cli_explain || env_explain.is_some_and(is_env_truthy);

        let mut pm_by_ecosystem = HashMap::new();
        if let Some(loaded) = config {
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
            fallback,
            explain,
        })
    }
}

fn is_env_truthy(raw: &str) -> bool {
    !raw.is_empty() && !matches!(raw, "0" | "false" | "no" | "off")
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
    if let Some(raw) = cli {
        return parse_fallback_label(raw);
    }
    if let Some(raw) = env.filter(|s| !s.is_empty()) {
        return parse_fallback_label(raw);
    }
    if let Some(loaded) = config
        && let Some(raw) = loaded.config.resolution.fallback.as_deref()
    {
        return parse_fallback_label(raw);
    }
    Ok(FallbackPolicy::default())
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

/// Generic CLI-then-env override parser. CLI wins; empty env strings are
/// treated as unset so a user can clear an inherited variable with
/// `RUNNER_PM= runner …`.
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
    if let Some(raw) = cli {
        let parsed = parse(raw)?;
        return Ok(Some(build(parsed, OverrideOrigin::CliFlag)));
    }
    match env {
        Some(raw) if !raw.is_empty() => {
            let parsed = parse(raw)?;
            Ok(Some(build(parsed, OverrideOrigin::EnvVar)))
        }
        _ => Ok(None),
    }
}

fn join_labels<I: Iterator<Item = &'static str>>(labels: I) -> String {
    labels.collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::{
        FallbackPolicy, OverrideOrigin, PmOverride, ResolutionOverrides, ResolutionStep, Resolver,
        RunnerOverride,
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

    fn resolver(ctx: &ProjectContext) -> Resolver<'_> {
        Resolver::new(ctx, ResolutionOverrides::default())
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
        let decision = resolver(&ctx)
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
        let decision = Resolver::new(&ctx, overrides)
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
        let err = Resolver::new(&ctx, overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail when nothing matches");

        let msg = format!("{err}");
        assert!(msg.contains("no Node package manager detected"));
        assert!(msg.contains("--pm"));
    }

    #[test]
    fn prefers_node_pm_over_non_node_primary() {
        let ctx = context(vec![PackageManager::Cargo, PackageManager::Bun]);
        let decision = resolver(&ctx)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_primary_pm_when_no_node_pm_detected() {
        let ctx = context(vec![PackageManager::Deno]);
        let decision = resolver(&ctx)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Deno);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn cli_override_beats_detected_pm() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Yarn, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, overrides)
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
        let decision = Resolver::new(&ctx, overrides)
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
        let decision = Resolver::new(&ctx, overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn cross_ecosystem_pm_override_is_ignored_for_node_scripts() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Cargo, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn cli_pm_value_parses_to_overrides() {
        let overrides =
            ResolutionOverrides::from_values(Some("yarn"), None, None, None, None, None, None)
                .expect("--pm yarn should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
        assert!(overrides.runner.is_none());
    }

    #[test]
    fn env_pm_value_parses_when_cli_absent() {
        let overrides =
            ResolutionOverrides::from_values(None, Some("bun"), None, None, None, None, None)
                .expect("RUNNER_PM=bun should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Bun);
        assert_eq!(pm.origin, OverrideOrigin::EnvVar);
    }

    #[test]
    fn cli_wins_over_env() {
        let overrides = ResolutionOverrides::from_values(
            Some("yarn"),
            Some("bun"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("both sources should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn empty_env_is_treated_as_unset() {
        let overrides =
            ResolutionOverrides::from_values(None, Some(""), None, None, None, None, None)
                .expect("empty env should parse as no override");

        assert!(overrides.pm.is_none());
    }

    #[test]
    fn cli_runner_value_parses_to_overrides() {
        let overrides =
            ResolutionOverrides::from_values(None, None, Some("just"), None, None, None, None)
                .expect("--runner just should parse");

        let runner: RunnerOverride = overrides.runner.expect("runner override should be present");
        assert_eq!(runner.runner, TaskRunner::Just);
        assert_eq!(runner.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn unknown_pm_label_errors_with_valid_value_list() {
        let err =
            ResolutionOverrides::from_values(Some("zoot"), None, None, None, None, None, None)
                .expect_err("unknown PM should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown package manager"));
        assert!(msg.contains("npm"));
        assert!(msg.contains("pnpm"));
    }

    #[test]
    fn unknown_runner_label_errors_with_valid_value_list() {
        let err =
            ResolutionOverrides::from_values(None, None, Some("zoot"), None, None, None, None)
                .expect_err("unknown runner should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown task runner"));
        assert!(msg.contains("turbo"));
    }

    #[test]
    fn bundler_alias_bundle_is_accepted() {
        let overrides =
            ResolutionOverrides::from_values(Some("bundle"), None, None, None, None, None, None)
                .expect("`bundle` should alias to bundler");

        assert_eq!(
            overrides.pm.expect("pm should be present").pm,
            PackageManager::Bundler,
        );
    }

    #[test]
    fn go_task_alias_is_accepted() {
        let overrides =
            ResolutionOverrides::from_values(None, None, Some("go-task"), None, None, None, None)
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

        let decision = Resolver::new(&ctx, overrides)
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

        let decision = Resolver::new(&ctx, overrides)
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
        let overrides =
            ResolutionOverrides::from_values(None, None, None, None, None, None, Some(&loaded))
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
        let overrides =
            ResolutionOverrides::from_values(None, None, None, None, None, None, Some(&loaded))
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
        let err =
            ResolutionOverrides::from_values(None, None, None, None, None, None, Some(&loaded))
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

        let decision = Resolver::new(&ctx, ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(decision.via, ResolutionStep::ManifestPackageManager);
        assert_eq!(decision.warnings.len(), 1);
        assert_eq!(decision.warnings[0].source, "package.json");
        assert!(
            decision.warnings[0].detail.contains("declaration wins"),
            "warning should mention declaration wins: {}",
            decision.warnings[0].detail,
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
        let decision = Resolver::new(&ctx, ResolutionOverrides::default())
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
        let decision = Resolver::new(&ctx, overrides)
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
        let decision = Resolver::new(&ctx, ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::ManifestPackageManager);
        assert!(decision.warnings.is_empty());
    }

    #[test]
    fn describe_renders_human_friendly_step_label() {
        let cli_decision = super::ResolvedPm {
            pm: PackageManager::Yarn,
            via: ResolutionStep::Override(OverrideOrigin::CliFlag),
            warnings: vec![],
        };
        assert!(cli_decision.describe().contains("--pm"));
        assert!(cli_decision.describe().contains("yarn"));

        let env_decision = super::ResolvedPm {
            pm: PackageManager::Bun,
            via: ResolutionStep::Override(OverrideOrigin::EnvVar),
            warnings: vec![],
        };
        assert!(env_decision.describe().contains("RUNNER_PM"));

        let pkg_decision = super::ResolvedPm {
            pm: PackageManager::Pnpm,
            via: ResolutionStep::ManifestPackageManager,
            warnings: vec![],
        };
        assert!(pkg_decision.describe().contains("packageManager"));

        let lock_decision = super::ResolvedPm {
            pm: PackageManager::Npm,
            via: ResolutionStep::Lockfile,
            warnings: vec![],
        };
        assert!(lock_decision.describe().contains("lockfile"));
    }

    #[test]
    fn deno_config_value_lands_under_deno_ecosystem_and_resolves_for_node_scripts() {
        // The runner.toml field is `[pm].node = "deno"`; the resolver
        // stores it under Ecosystem::Deno (per PackageManager::ecosystem)
        // and the Node-script resolver consults both Node and Deno keys.
        let loaded = loaded_config_with_node("deno");
        let overrides =
            ResolutionOverrides::from_values(None, None, None, None, None, None, Some(&loaded))
                .expect("deno config should parse");

        assert!(overrides.pm_by_ecosystem.contains_key(&Ecosystem::Deno));

        let ctx = context(vec![PackageManager::Pnpm]);
        let decision = Resolver::new(&ctx, overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");
        assert_eq!(decision.pm, PackageManager::Deno);
    }
}
