//! Resolution of package managers and task sources for `runner run`.
//!
//! The resolver consumes a [`ProjectContext`] (signals discovered during
//! detection) and returns a single decision plus a [`ResolutionStep`]
//! describing which signal produced it. This module is the canonical place
//! to extend the precedence chain in later phases:
//!
//! 1. Qualified syntax (`turbo.json:build`) — handled in `cmd::run` today.
//! 2. CLI flag (`--pm`, `--runner`).
//! 3. Environment variable (`RUNNER_PM`, `RUNNER_RUNNER`).
//! 4. Project config (`./runner.toml`).
//! 5. Manifest declaration (`packageManager`, `devEngines.packageManager`).
//! 6. Lockfile (current behavior; lives in [`crate::detect`]).
//! 7. `PATH` probe in canonical order.
//! 8. Terminal — error with actionable guidance.
//!
//! Phase 1 only implements step 6 (lockfile) and the legacy `npm` fallback,
//! so callers receive the same decision they did before the refactor.

use crate::types::{PackageManager, ProjectContext};

/// Resolves package managers and task sources from a [`ProjectContext`].
///
/// Holds an immutable borrow of the context so callers can build it once per
/// dispatch and reuse it across resolution queries.
pub(crate) struct Resolver<'ctx> {
    ctx: &'ctx ProjectContext,
}

/// A package-manager decision plus the chain step that produced it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedPm {
    /// The chosen package manager.
    pub pm: PackageManager,
    /// Which step of the resolution chain produced [`Self::pm`]. Consumed by
    /// the diagnostic surface added in a later phase (`--explain` /
    /// `runner why`); kept on the result type now so resolver internals can
    /// already populate it without churning the public API later.
    #[allow(dead_code, reason = "consumed by --explain in a later phase")]
    pub via: ResolutionStep,
}

/// Which step of the resolution chain produced a decision.
///
/// Phase 1 carries only the steps reachable today. Later phases extend this
/// enum; downstream `match` sites stay exhaustive on purpose so that adding
/// a step is a compile error to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionStep {
    /// Step 6 — package manager inferred from a lockfile (or another
    /// detector recorded in [`ProjectContext::package_managers`]).
    Lockfile,
    /// Step 8 (legacy) — no signals matched, default to `npm` so that
    /// `runner run <script>` still has a chance to dispatch. Phase 5 replaces
    /// this with a `PATH` probe and an error when nothing is available.
    LegacyNpmFallback,
}

impl<'ctx> Resolver<'ctx> {
    /// Wrap a project context.
    pub(crate) const fn new(ctx: &'ctx ProjectContext) -> Self {
        Self { ctx }
    }

    /// Resolve the package manager used to dispatch `package.json` scripts.
    ///
    /// Mirrors the historical decision made inline in
    /// `cmd::run::build_run_command`: prefer a detected Node-ecosystem
    /// package manager, fall back to the primary PM of any ecosystem
    /// (so a `packageManager: "deno@2"` declaration still wins), and as a
    /// final resort default to `npm` so existing behavior is preserved.
    pub(crate) fn resolve_node_pm(&self) -> ResolvedPm {
        self.ctx
            .primary_node_pm()
            .or_else(|| self.ctx.primary_pm())
            .map_or(
                ResolvedPm {
                    pm: PackageManager::Npm,
                    via: ResolutionStep::LegacyNpmFallback,
                },
                |pm| ResolvedPm {
                    pm,
                    via: ResolutionStep::Lockfile,
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ResolutionStep, Resolver};
    use crate::types::{PackageManager, ProjectContext};

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

    #[test]
    fn resolves_detected_node_pm_via_lockfile() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let decision = Resolver::new(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_legacy_npm_when_no_pm_detected() {
        let ctx = context(vec![]);
        let decision = Resolver::new(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Npm);
        assert_eq!(decision.via, ResolutionStep::LegacyNpmFallback);
    }

    #[test]
    fn prefers_node_pm_over_non_node_primary() {
        let ctx = context(vec![PackageManager::Cargo, PackageManager::Bun]);
        let decision = Resolver::new(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_primary_pm_when_no_node_pm_detected() {
        // Deno is not `is_node()` but is still a valid script dispatcher
        // when declared via `packageManager: "deno@2"`.
        let ctx = context(vec![PackageManager::Deno]);
        let decision = Resolver::new(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Deno);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }
}
