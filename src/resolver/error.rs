//! Typed error variants produced by the resolution chain.
//!
//! The resolver returns `Result<_, ResolveError>` so callers can match on
//! the failure mode without parsing prose, and so `main` can map specific
//! variants to distinct exit codes (`ResolveError` → 2, every other
//! `anyhow::Error` → 1). The plan/spec calls this out in Appendix A.6
//! and A.8: a clean, machine-readable boundary between "resolver said no"
//! and "something else broke".
//!
//! Wherever a caller wants to bubble up through `anyhow`, the variant
//! converts automatically because `ResolveError` implements
//! `std::error::Error` — `?` works, and `main` recovers the variant via
//! `err.downcast_ref::<ResolveError>()` to decide the exit code.

use std::fmt;

use crate::types::{Ecosystem, PackageManager};

/// A resolver-side failure. Distinct from `anyhow::Error` so the
/// terminal exit-code mapping in `main` can treat resolver failures as a
/// hard "intent could not be satisfied" (exit 2) rather than a generic
/// internal error (exit 1).
#[derive(Debug)]
pub(crate) enum ResolveError {
    /// No signals matched and the active fallback policy could not pick a
    /// package manager.
    ///
    /// `soft = true` is emitted by `FallbackPolicy::Probe` when `$PATH`
    /// holds nothing usable. `cmd::run` treats it as a "fall through to
    /// a direct PATH spawn" signal so `runner run somebin` keeps
    /// working in projects with no PM signals at all. `soft = false`
    /// fires under `FallbackPolicy::Error` and propagates straight to
    /// the user.
    NoSignalsFound {
        /// Which ecosystem the resolver was trying to satisfy.
        ecosystem: Ecosystem,
        /// `true` if a direct-spawn fallback is allowed; `false` if the
        /// caller should treat the missing PM as fatal.
        soft: bool,
    },
    /// `devEngines.packageManager` `onFail = error` rejected the
    /// installed environment — either the declared binary is missing or
    /// its version doesn't satisfy the declared range.
    DevEnginesFailHard {
        /// The PM the manifest declared.
        pm: PackageManager,
        /// Whether the binary was missing or the version mismatched.
        reason: DevEnginesFailReason,
    },
    /// `--on-mismatch error` (or `[resolution].on_mismatch = "error"`)
    /// was set and a manifest declaration disagrees with the detected
    /// lockfile. Phase A1 will populate this; B2 introduces the variant
    /// so the exit-code mapping is wired up before the policy lands.
    MismatchPolicyError {
        /// The PM the manifest declared.
        declared: PackageManager,
        /// Which manifest field carried the declaration (`"packageManager"`
        /// or `"devEngines.packageManager"`).
        field: &'static str,
        /// The PM the lockfile points to.
        lockfile: PackageManager,
    },
    /// A user-supplied override (CLI flag, env var, or config) names a
    /// PM that can't satisfy the requested resolution — e.g. `--pm cargo`
    /// when the call is dispatching a `package.json` script. Phase B5
    /// will start emitting this; B2 introduces the variant.
    InvalidOverride {
        /// Raw value the user supplied (`"cargo"`, `"poetry"`, …).
        value: String,
        /// Static reason string for the diagnostic. Variant kept short
        /// so the `Display` impl produces a clean one-line message.
        reason: &'static str,
    },
    /// Both `keep_going` and `kill_on_fail` were set to true at the same
    /// source (or once layered across CLI/env/config). The chain executor
    /// can't honour both, so fail loudly before dispatching anything.
    ConflictingFailurePolicy {
        /// Where the conflict was detected: `"CLI flags"`, `"env vars"`,
        /// `"[chain] config"`, or `"cross-source"`.
        source: &'static str,
    },
}

/// Why a `devEngines.packageManager` `onFail = error` check rejected the
/// environment. Carried by [`ResolveError::DevEnginesFailHard`] so
/// `--explain` and `doctor` can attribute the failure precisely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DevEnginesFailReason {
    /// The declared PM is not on `$PATH`.
    BinaryMissing,
    /// The declared range doesn't include the installed version.
    VersionMismatch {
        /// Declared range, as written (e.g. `"^9.0.0"`).
        declared: String,
        /// Actual `--version` output of the installed binary.
        actual: String,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSignalsFound { ecosystem, soft } => {
                let suffix = if *soft { "" } else { " (--fallback=error)" };
                write!(
                    f,
                    "no {} package manager detected{suffix}. Checked: lockfiles, manifest \
                     (packageManager + devEngines), PATH. Pin one with `--pm <name>`, set \
                     `RUNNER_PM=<name>`, add it to runner.toml, or install a supported PM.",
                    ecosystem.label(),
                )
            }
            Self::DevEnginesFailHard { pm, reason } => match reason {
                DevEnginesFailReason::BinaryMissing => write!(
                    f,
                    "devEngines.packageManager declares {} but it was not found on PATH \
                     (onFail=error)",
                    pm.label(),
                ),
                DevEnginesFailReason::VersionMismatch { declared, actual } => write!(
                    f,
                    "devEngines.packageManager requires {} {declared} but the installed version \
                     is {actual} (onFail=error)",
                    pm.label(),
                ),
            },
            Self::MismatchPolicyError {
                declared,
                field,
                lockfile,
            } => write!(
                f,
                "{field} declares {} but the lockfile reflects {} (--on-mismatch=error)",
                declared.label(),
                lockfile.label(),
            ),
            Self::InvalidOverride { value, reason } => {
                write!(f, "invalid override value {value:?}: {reason}")
            }
            Self::ConflictingFailurePolicy { source } => write!(
                f,
                "`keep_going` and `kill_on_fail` are mutually exclusive but both were set ({source}). \
                 Unset one of `--keep-going` / `RUNNER_KEEP_GOING` / `[chain].keep_going` or \
                 `--kill-on-fail` / `RUNNER_KILL_ON_FAIL` / `[chain].kill_on_fail` to pick a policy.",
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicting_failure_policy_display_includes_source() {
        let err = ResolveError::ConflictingFailurePolicy { source: "env vars" };
        let msg = format!("{err}");
        assert!(msg.contains("keep_going"), "msg: {msg}");
        assert!(msg.contains("kill_on_fail"), "msg: {msg}");
        assert!(msg.contains("env vars"), "msg: {msg}");
    }
}
