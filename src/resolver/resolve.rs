//! Resolution algorithm: the `impl Resolver` block plus the manifest or lockfile cross-checks that feed it.
//!
//! Pure logic only. Parsing user input lives in [`super::overrides`] and
//! [`super::policies`]; data types live in [`super::types`].

use super::probe;
use super::types::{FallbackPolicy, MismatchPolicy, ResolutionStep, ResolvedPm, Resolver};
use super::{DevEnginesFailReason, ResolveError};
use crate::tool::node::{
    ManifestPmDecl, ManifestSource, OnFail, VersionCheck, check_version_constraint,
    detect_pm_from_manifest, find_manifest_upwards,
};
use crate::types::{DetectionWarning, Ecosystem, PackageManager, ProjectContext};

impl<'ctx> Resolver<'ctx> {
    /// Wrap a project context plus the override bundle for this invocation.
    pub(crate) const fn new(
        ctx: &'ctx ProjectContext,
        overrides: &'ctx super::types::ResolutionOverrides,
    ) -> Self {
        Self { ctx, overrides }
    }

    /// Resolve the package manager used to dispatch `package.json` scripts.
    ///
    /// Walks the precedence chain in order:
    /// - Step 2–3, CLI/env PM override (when compatible with Node scripts).
    /// - Step 4, `runner.toml` `[pm].node` override.
    /// - Step 5a, `package.json` legacy `packageManager` field.
    /// - Step 5b, `package.json` `devEngines.packageManager` field
    ///   (honoring `onFail` when the declared PM is missing from PATH).
    /// - Step 6, lockfile (via [`ProjectContext::primary_node_pm`]).
    /// - Step 7, `$PATH` probe in canonical Node order
    ///   (`bun > pnpm > yarn > npm`). Active by default; replaced by
    ///   step 8 when `--fallback npm` is set.
    /// - Step 8, error or legacy `npm` (depending on
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
                // would silently disregard their intent. Surface the
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
        // Node lockfile step. Otherwise a mixed-language repo whose
        // top-priority signal is `Cargo.lock` would pick Cargo and bail
        // later, instead of continuing to the PATH probe / fallback.
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
                // Don't probe Node PMs without Node-ecosystem evidence.
                // No `package.json` upward means this isn't a Node
                // project, so picking `bun`/`pnpm`/`yarn`/`npm` off
                // `$PATH` would dispatch through the wrong ecosystem.
                if find_manifest_upwards(&self.ctx.root).is_none() {
                    return Err(no_pm_found_soft());
                }
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
/// - `Ignore`, no check.
/// - `Warn`, emit a `package.json` warning when the PM is missing or
///   the version doesn't match; continue with the declared PM regardless.
/// - `Error`, bail on a missing PM or a version mismatch.
///
/// Version checks that can't run (unparseable range, missing
/// `--version` output, etc.) are skipped silently: the proposal says
/// `onFail` enforces user intent, but blocking dispatch on an
/// unverifiable constraint would be worse than continuing; the binary
/// will surface the real problem at spawn time.
///
/// Binary-presence and version-check side effects are injected so the
/// `Error` branches stay exercisable in unit tests: `Error + missing`
/// and `Error + mismatched version` both `bail!`, which is impossible
/// to cover otherwise without controlling the host `$PATH` and running
/// `<pm> --version` against a real binary. Production callers wire in
/// [`real_binary_check`] and [`check_version_constraint`].
pub(super) fn apply_manifest_on_fail<P, V>(
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

/// Soft "no PM found", only emitted from the `Probe` fallback when
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

/// Hard "no PM found", emitted from `FallbackPolicy::Error`. Carries
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
/// - [`MismatchPolicy::Warn`], push a `PmMismatch` warning; declaration wins.
/// - [`MismatchPolicy::Ignore`], declaration wins silently.
/// - [`MismatchPolicy::Error`], bail with
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
