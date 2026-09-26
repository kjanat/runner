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
//! `std::error::Error`, `?` works, and `main` recovers the variant via
//! `err.downcast_ref::<ResolveError>()` to decide the exit code.

use std::fmt;

use crate::provider::Named;
use runner_core::ProviderId;

/// A resolver-side failure. Distinct from `anyhow::Error` so the
/// terminal exit-code mapping in `main` can treat resolver failures as a
/// hard "intent could not be satisfied" (exit 2) rather than a generic
/// internal error (exit 1).
#[derive(Debug)]
pub(crate) enum ResolveError {
    /// A read-only provider query failed.
    Observation(std::io::Error),
    /// No provider has an observed installation capability.
    NoInstallers,
    /// A `--pm` / `RUNNER_PM` override names a PM that detection did not
    /// find in the project, so `runner install` cannot honor it. Erroring
    /// (rather than silently installing with the detected set) keeps the
    /// override a contract: what the user pinned is what runs.
    PmOverrideNotDetected {
        /// The PM the override named.
        pm: ProviderId,
        /// Where the override came from (flag, env var, config file).
        origin: super::types::OverrideOrigin,
        /// What detection actually found, for the error message.
        detected: Vec<ProviderId>,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observation(error) => write!(f, "{error}"),
            Self::NoInstallers => f.write_str("no observed provider can install dependencies"),
            Self::PmOverrideNotDetected {
                pm,
                origin,
                detected,
            } => {
                let detected = if detected.is_empty() {
                    "none".to_string()
                } else {
                    detected
                        .iter()
                        .map(|pm| pm.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                write!(
                    f,
                    "cannot install with {} {}: not a detected package manager in this project \
                     (detected: {detected}). Install {} or drop the override.",
                    pm.label(),
                    origin.describe_pm_source(),
                    pm.label(),
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_override_not_detected_display_names_source_and_detected() {
        let err = ResolveError::PmOverrideNotDetected {
            pm: ProviderId::Pnpm,
            origin: super::super::types::OverrideOrigin::EnvVar,
            detected: vec![ProviderId::Npm, ProviderId::Cargo],
        };
        let msg = format!("{err}");
        assert!(msg.contains("pnpm"), "msg: {msg}");
        assert!(msg.contains("RUNNER_PM"), "msg: {msg}");
        assert!(msg.contains("npm, cargo"), "msg: {msg}");
    }

    #[test]
    fn pm_override_not_detected_display_handles_empty_detected() {
        let err = ResolveError::PmOverrideNotDetected {
            pm: ProviderId::Pnpm,
            origin: super::super::types::OverrideOrigin::CliFlag,
            detected: Vec::new(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("detected: none"), "msg: {msg}");
        assert!(msg.contains("--pm"), "msg: {msg}");
    }
}
