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

use crate::types::PackageManager;

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
        pm: PackageManager,
        /// Where the override came from (flag, env var, config file).
        origin: super::types::OverrideOrigin,
        /// What detection actually found, for the error message.
        detected: Vec<PackageManager>,
    },
    /// Both `keep_going` and `kill_on_fail` were set to true at the same
    /// source (or once layered across CLI/env/config). The chain executor
    /// can't honour both, so fail loudly before dispatching anything.
    ConflictingFailurePolicy {
        /// Where the conflict was detected: `"CLI flags"`, `"env vars"`,
        /// `"[chain] config"`, or `"cross-source"`.
        source: &'static str,
    },
    /// `[install].on_collision = "error"` and the install set holds two or
    /// more package managers that write the same directory.
    InstallDirCollision {
        /// The shared directory, e.g. `"node_modules"`.
        dir: &'static str,
        /// The colliding writers, in detection order.
        writers: Vec<PackageManager>,
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
            Self::ConflictingFailurePolicy { source } => write!(
                f,
                "`keep_going` and `kill_on_fail` are mutually exclusive but both were set \
                 ({source}). Unset one of `--keep-going` / `RUNNER_KEEP_GOING` / \
                 `[chain].keep_going` or `--kill-on-fail` / `RUNNER_KILL_ON_FAIL` / \
                 `[chain].kill_on_fail` to pick a policy.",
            ),
            Self::InstallDirCollision { dir, writers } => {
                write!(f, "{}", install_dir_collision(dir, writers))
            }
        }
    }
}

fn install_dir_collision(dir: &str, writers: &[PackageManager]) -> String {
    let list = writers
        .iter()
        .map(|pm| pm.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{list} all install into {dir}/ and `[install].on_collision = \"error\"` refuses to run \
         two writers over one tree. Disable an installer with `[tools.<name>].install = false`, \
         or drop `on_collision` to let runner resolve it.",
    )
}

impl std::error::Error for ResolveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_override_not_detected_display_names_source_and_detected() {
        let err = ResolveError::PmOverrideNotDetected {
            pm: PackageManager::Pnpm,
            origin: super::super::types::OverrideOrigin::EnvVar,
            detected: vec![PackageManager::Npm, PackageManager::Cargo],
        };
        let msg = format!("{err}");
        assert!(msg.contains("pnpm"), "msg: {msg}");
        assert!(msg.contains("RUNNER_PM"), "msg: {msg}");
        assert!(msg.contains("npm, cargo"), "msg: {msg}");
    }

    #[test]
    fn pm_override_not_detected_display_handles_empty_detected() {
        let err = ResolveError::PmOverrideNotDetected {
            pm: PackageManager::Pnpm,
            origin: super::super::types::OverrideOrigin::CliFlag,
            detected: Vec::new(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("detected: none"), "msg: {msg}");
        assert!(msg.contains("--pm"), "msg: {msg}");
    }

    #[test]
    fn conflicting_failure_policy_display_includes_source() {
        let err = ResolveError::ConflictingFailurePolicy { source: "env vars" };
        let msg = format!("{err}");
        assert!(msg.contains("keep_going"), "msg: {msg}");
        assert!(msg.contains("kill_on_fail"), "msg: {msg}");
        assert!(msg.contains("env vars"), "msg: {msg}");
    }
}
