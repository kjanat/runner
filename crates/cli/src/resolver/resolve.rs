//! Resolution algorithm: the `impl Resolver` block plus the manifest or lockfile cross-checks that feed it.
//!
//! Pure logic only. Parsing user input lives in [`super::overrides`] and
//! [`super::policies`]; data types live in [`super::types`].

use super::probe;
use super::types::{MismatchPolicy, ResolutionStep, ResolvedPm, Resolver};
use super::{DevEnginesFailReason, ResolveError};
use crate::tool::node::{
    ManifestPmDecl, ManifestSource, OnFail, VersionCheck, check_version_constraint,
    detect_pm_from_manifest,
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

    /// Resolve the package manager accepting package scripts.
    pub(crate) fn resolve_node_pm(&self) -> Result<ResolvedPm, ResolveError> {
        let policy = crate::commands::run::core::policy(self.overrides);
        let project = crate::commands::run::core::project_under(self.ctx, &policy)
            .map_err(ResolveError::Observation)?;
        self.resolve_node_pm_in(&project)
    }

    /// Describe the selection from the invocation's resolved provider snapshot.
    pub(crate) fn resolve_node_pm_in(
        &self,
        project: &runner_core::Project,
    ) -> Result<ResolvedPm, ResolveError> {
        let registry = &runner_providers::REGISTRY;
        let tree = crate::commands::run::core::tree(self.ctx);
        let scope = runner_core::plan::scope_at(&tree, &tree.cwd);
        let chosen = self
            .overrides
            .pm
            .as_ref()
            .or_else(|| self.overrides.pm_by_ecosystem.get(&Ecosystem::Node));
        if let Some(chosen) = chosen {
            let descriptor = registry
                .by_label(chosen.pm.label())
                .expect("package manager registry entry");
            if !descriptor
                .caps
                .run_task
                .is_some_and(|cap| cap.sources.contains(&runner_core::ProviderId::PackageJson))
            {
                return Err(ResolveError::InvalidOverride {
                    value: chosen.pm.label().into(),
                    reason: "cannot dispatch package.json scripts",
                });
            }
            if project.present_in(descriptor.id, &scope).is_none() {
                return Err(ResolveError::InvalidOverride {
                    value: chosen.pm.label().into(),
                    reason: "no file or executable was observed for this provider",
                });
            }
        }
        let mut warnings = Vec::new();
        let declaration = chosen
            .is_none()
            .then(|| detect_pm_from_manifest(&self.ctx.root))
            .flatten();
        if let Some(decl) = &declaration {
            cross_check_against_lockfile(
                decl,
                self.ctx,
                self.overrides.on_mismatch,
                &mut warnings,
            )?;
            apply_manifest_on_fail(
                decl,
                &mut warnings,
                real_binary_check,
                check_version_constraint,
            )?;
        }
        let present = project
            .for_source(
                runner_core::ProviderId::PackageJson,
                &scope,
                &crate::commands::run::core::policy(self.overrides),
                registry,
            )
            .ok_or_else(no_pm_found_soft)?;
        let pm = PackageManager::from_label(registry.by_id(present.provider).label)
            .expect("package manager label");
        let via = if let Some(chosen) = chosen {
            ResolutionStep::Override(chosen.origin.clone())
        } else if let Some(decl) = declaration.filter(|decl| decl.pm == pm) {
            match decl.source {
                ManifestSource::PackageManager => ResolutionStep::ManifestPackageManager,
                ManifestSource::DevEngines => ResolutionStep::ManifestDevEngines {
                    on_fail: decl.on_fail,
                },
            }
        } else {
            let evidence = present.because.first().expect("resolved provider evidence");
            if evidence.weight == runner_core::Weight::Probed {
                warnings.push(DetectionWarning::PathProbeFallback {
                    picked: pm,
                    ecosystem: Ecosystem::Node,
                    others_available: Vec::new(),
                });
                ResolutionStep::PathProbe {
                    binary: evidence.at.clone(),
                }
            } else {
                ResolutionStep::Observed {
                    path: evidence.at.clone(),
                    weight: evidence.weight,
                }
            }
        };
        Ok(ResolvedPm { pm, via, warnings })
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
/// `--version` output, etc.) produce a warning.
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

    if let Some(range) = decl.version.as_deref() {
        match check_version(decl.pm, range) {
            VersionCheck::Mismatch { declared, actual } => {
                return on_fail_version_mismatch(decl, &declared, &actual, warnings);
            }
            VersionCheck::Unverifiable { reason } => {
                warnings.push(DetectionWarning::TaskListUnreadable {
                    source: "package.json",
                    error: format!(
                        "cannot evaluate {} version constraint {range}: {reason}",
                        decl.pm.label()
                    ),
                })
            }
            VersionCheck::Satisfied => {}
        }
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
/// through to a direct PATH spawn (`commands::run::run_pm_exec_fallback`)
/// match on `ResolveError::NoSignalsFound { soft: true, .. }` and swallow
/// it; every other resolver error surfaces to the user.
const fn no_pm_found_soft() -> ResolveError {
    ResolveError::NoSignalsFound {
        ecosystem: Ecosystem::Node,
        soft: true,
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
