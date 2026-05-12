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

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::config::{LoadedConfig, parse_node_pm, parse_python_pm};
use crate::types::{Ecosystem, PackageManager, ProjectContext, TaskRunner};

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
}

/// Which step of the resolution chain produced a decision.
///
/// Listed in precedence order. Downstream `match` sites stay exhaustive so
/// that adding a step is a compile error to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolutionStep {
    /// Steps 2–4 — user-supplied override won.
    Override(OverrideOrigin),
    /// Step 6 — package manager inferred from a lockfile (or another
    /// detector recorded in [`ProjectContext::package_managers`]).
    Lockfile,
    /// Step 8 (legacy) — no signals matched, default to `npm` so that
    /// `runner run <script>` still has a chance to dispatch. Phase 5 replaces
    /// this with a `PATH` probe and an error when nothing is available.
    LegacyNpmFallback,
}

impl<'ctx> Resolver<'ctx> {
    /// Wrap a project context plus the override bundle for this invocation.
    pub(crate) const fn new(ctx: &'ctx ProjectContext, overrides: ResolutionOverrides) -> Self {
        Self { ctx, overrides }
    }

    /// Resolve the package manager used to dispatch `package.json` scripts.
    ///
    /// 1. Honor a CLI/env PM override (step 2-3) iff the chosen PM can
    ///    actually run `package.json` scripts (Node ecosystems plus Deno).
    /// 2. Honor a `runner.toml` `[pm].node` override (step 4).
    /// 3. Otherwise, prefer a detected Node-ecosystem PM (step 6), fall back
    ///    to the primary PM of any ecosystem (so a
    ///    `packageManager: "deno@2"` declaration still wins), and as a
    ///    final resort default to `npm`.
    pub(crate) fn resolve_node_pm(&self) -> ResolvedPm {
        if let Some(o) = self.overrides.pm.as_ref()
            && pm_can_run_package_json_scripts(o.pm)
        {
            return ResolvedPm {
                pm: o.pm,
                via: ResolutionStep::Override(o.origin.clone()),
            };
        }
        if let Some(o) = self
            .overrides
            .pm_by_ecosystem
            .get(&Ecosystem::Node)
            .or_else(|| self.overrides.pm_by_ecosystem.get(&Ecosystem::Deno))
        {
            return ResolvedPm {
                pm: o.pm,
                via: ResolutionStep::Override(o.origin.clone()),
            };
        }

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

const fn pm_can_run_package_json_scripts(pm: PackageManager) -> bool {
    pm.is_node() || matches!(pm, PackageManager::Deno)
}

impl ResolutionOverrides {
    /// Assemble overrides from CLI flag values (already parsed by clap),
    /// the `RUNNER_PM` / `RUNNER_RUNNER` environment variables, and an
    /// optional `runner.toml` loaded from the project root.
    ///
    /// Reads `std::env` for the env-var sources; pure parsing happens in
    /// [`Self::from_values`].
    ///
    /// # Errors
    ///
    /// Returns an error if any value does not name a known package manager
    /// or task runner, or if a `runner.toml` field contains a PM that does
    /// not belong to its target ecosystem.
    pub(crate) fn from_cli_and_env(
        cli_pm: Option<&str>,
        cli_runner: Option<&str>,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let env_pm = std::env::var("RUNNER_PM").ok();
        let env_runner = std::env::var("RUNNER_RUNNER").ok();
        Self::from_values(
            cli_pm,
            env_pm.as_deref(),
            cli_runner,
            env_runner.as_deref(),
            config,
        )
    }

    /// Pure-function variant of [`Self::from_cli_and_env`]. CLI values win
    /// over env values; empty env strings are treated as unset.
    ///
    /// # Errors
    ///
    /// Returns an error if any value does not name a known package manager
    /// or task runner, or if a `runner.toml` field contains a PM that does
    /// not belong to its target ecosystem.
    pub(crate) fn from_values(
        cli_pm: Option<&str>,
        env_pm: Option<&str>,
        cli_runner: Option<&str>,
        env_runner: Option<&str>,
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
        })
    }
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
        OverrideOrigin, PmOverride, ResolutionOverrides, ResolutionStep, Resolver, RunnerOverride,
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
            pm_by_ecosystem: HashMap::new(),
            runner: None,
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
            pm: None,
            pm_by_ecosystem: map,
            runner: None,
        }
    }

    #[test]
    fn resolves_detected_node_pm_via_lockfile() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let decision = resolver(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_legacy_npm_when_no_pm_detected() {
        let ctx = context(vec![]);
        let decision = resolver(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Npm);
        assert_eq!(decision.via, ResolutionStep::LegacyNpmFallback);
    }

    #[test]
    fn prefers_node_pm_over_non_node_primary() {
        let ctx = context(vec![PackageManager::Cargo, PackageManager::Bun]);
        let decision = resolver(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_primary_pm_when_no_node_pm_detected() {
        let ctx = context(vec![PackageManager::Deno]);
        let decision = resolver(&ctx).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Deno);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn cli_override_beats_detected_pm() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Yarn, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();

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
        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();

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
        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn cross_ecosystem_pm_override_is_ignored_for_node_scripts() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Cargo, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn cli_pm_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_values(Some("yarn"), None, None, None, None)
            .expect("--pm yarn should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
        assert!(overrides.runner.is_none());
    }

    #[test]
    fn env_pm_value_parses_when_cli_absent() {
        let overrides = ResolutionOverrides::from_values(None, Some("bun"), None, None, None)
            .expect("RUNNER_PM=bun should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Bun);
        assert_eq!(pm.origin, OverrideOrigin::EnvVar);
    }

    #[test]
    fn cli_wins_over_env() {
        let overrides =
            ResolutionOverrides::from_values(Some("yarn"), Some("bun"), None, None, None)
                .expect("both sources should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn empty_env_is_treated_as_unset() {
        let overrides = ResolutionOverrides::from_values(None, Some(""), None, None, None)
            .expect("empty env should parse as no override");

        assert!(overrides.pm.is_none());
    }

    #[test]
    fn cli_runner_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_values(None, None, Some("just"), None, None)
            .expect("--runner just should parse");

        let runner: RunnerOverride = overrides.runner.expect("runner override should be present");
        assert_eq!(runner.runner, TaskRunner::Just);
        assert_eq!(runner.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn unknown_pm_label_errors_with_valid_value_list() {
        let err = ResolutionOverrides::from_values(Some("zoot"), None, None, None, None)
            .expect_err("unknown PM should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown package manager"));
        assert!(msg.contains("npm"));
        assert!(msg.contains("pnpm"));
    }

    #[test]
    fn unknown_runner_label_errors_with_valid_value_list() {
        let err = ResolutionOverrides::from_values(None, None, Some("zoot"), None, None)
            .expect_err("unknown runner should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown task runner"));
        assert!(msg.contains("turbo"));
    }

    #[test]
    fn bundler_alias_bundle_is_accepted() {
        let overrides = ResolutionOverrides::from_values(Some("bundle"), None, None, None, None)
            .expect("`bundle` should alias to bundler");

        assert_eq!(
            overrides.pm.expect("pm should be present").pm,
            PackageManager::Bundler,
        );
    }

    #[test]
    fn go_task_alias_is_accepted() {
        let overrides = ResolutionOverrides::from_values(None, None, Some("go-task"), None, None)
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

        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();

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

        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();

        assert_eq!(decision.pm, PackageManager::Bun);
        match decision.via {
            ResolutionStep::Override(OverrideOrigin::CliFlag) => {}
            other => panic!("expected CliFlag origin, got {other:?}"),
        }
    }

    #[test]
    fn config_loaded_value_populates_pm_by_ecosystem() {
        let loaded = loaded_config_with_node("bun");
        let overrides = ResolutionOverrides::from_values(None, None, None, None, Some(&loaded))
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
        let overrides = ResolutionOverrides::from_values(None, None, None, None, Some(&loaded))
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
        let err = ResolutionOverrides::from_values(None, None, None, None, Some(&loaded))
            .expect_err("cargo is not a node-script PM");
        assert!(format!("{err}").contains("cannot dispatch package.json scripts"));
    }

    #[test]
    fn deno_config_value_lands_under_deno_ecosystem_and_resolves_for_node_scripts() {
        // The runner.toml field is `[pm].node = "deno"`; the resolver
        // stores it under Ecosystem::Deno (per PackageManager::ecosystem)
        // and the Node-script resolver consults both Node and Deno keys.
        let loaded = loaded_config_with_node("deno");
        let overrides = ResolutionOverrides::from_values(None, None, None, None, Some(&loaded))
            .expect("deno config should parse");

        assert!(overrides.pm_by_ecosystem.contains_key(&Ecosystem::Deno));

        let ctx = context(vec![PackageManager::Pnpm]);
        let decision = Resolver::new(&ctx, overrides).resolve_node_pm();
        assert_eq!(decision.pm, PackageManager::Deno);
    }
}
