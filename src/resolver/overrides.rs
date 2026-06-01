//! Override construction — `impl ResolutionOverrides` plus the CLI/env
//! parsers that feed it. Policy parsing lives in [`super::policies`];
//! the data shapes live in [`super::types`].

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use super::join_labels;
use super::policies::{
    is_env_truthy, parse_prefer_runners, resolve_failure_policy, resolve_fallback_policy,
    resolve_mismatch_policy,
};
use super::types::{
    DiagnosticFlags, ExplainSource, OverrideOrigin, OverrideSources, PmOverride,
    ResolutionOverrides, RunnerOverride, SourceValue,
};
use crate::config::{LoadedConfig, parse_node_pm, parse_python_pm};
use crate::types::{Ecosystem, PackageManager, TaskRunner};

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
        diagnostics: DiagnosticFlags,
        failure: crate::cli::ChainFailureFlags,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let env_pm = std::env::var("RUNNER_PM").ok();
        let env_runner = std::env::var("RUNNER_RUNNER").ok();
        let env_fallback = std::env::var("RUNNER_FALLBACK").ok();
        let env_on_mismatch = std::env::var("RUNNER_ON_MISMATCH").ok();
        let env_no_warnings = std::env::var("RUNNER_NO_WARNINGS").ok();
        let env_explain = std::env::var("RUNNER_EXPLAIN").ok();
        let env_keep_going = std::env::var("RUNNER_KEEP_GOING").ok();
        let env_kill_on_fail = std::env::var("RUNNER_KILL_ON_FAIL").ok();
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
                cli: diagnostics.no_warnings,
                env: env_no_warnings.as_deref(),
            },
            explain: ExplainSource {
                cli: diagnostics.explain,
                env: env_explain.as_deref(),
            },
            keep_going: ExplainSource {
                cli: failure.keep_going,
                env: env_keep_going.as_deref(),
            },
            kill_on_fail: ExplainSource {
                cli: failure.kill_on_fail,
                env: env_kill_on_fail.as_deref(),
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
        let failure_policy =
            resolve_failure_policy(sources.keep_going, sources.kill_on_fail, sources.config)?;
        // Output grouping toggles (no CLI/env layer in v1). `group_output`
        // (default true) is the broad GitHub Actions grouping switch.
        // Parallel grouping diverges by environment: `github_group_parallel`
        // (default true) applies under Actions only when `group_output` is
        // also true; `parallel_grouped` (default false) applies elsewhere.
        let group_output = sources.config.is_none_or(|c| c.config.github.group_output);
        let github_group_parallel = sources
            .config
            .is_none_or(|c| c.config.github.group_parallel);
        let parallel_grouped = sources.config.is_some_and(|c| c.config.parallel.grouped);

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
            failure_policy,
            group_output,
            github_group_parallel,
            parallel_grouped,
        })
    }
}

fn parse_pm_label(raw: &str) -> Result<PackageManager> {
    if let Some(pm) = PackageManager::from_label(raw) {
        return Ok(pm);
    }
    if let Some(runner) = TaskRunner::from_label(raw) {
        return Err(anyhow!(
            "{:?} is a task runner, not a package manager; use `--runner {}` instead",
            raw,
            runner.label(),
        ));
    }
    Err(anyhow!(
        "unknown package manager {raw:?}; expected one of {}",
        join_labels(
            PackageManager::all()
                .iter()
                .copied()
                .map(PackageManager::label)
        ),
    ))
}

fn parse_runner_label(raw: &str) -> Result<TaskRunner> {
    if let Some(runner) = TaskRunner::from_label(raw) {
        return Ok(runner);
    }
    if let Some(pm) = PackageManager::from_label(raw) {
        return Err(anyhow!(
            "{:?} is a package manager, not a task runner; use `--pm {}` instead",
            raw,
            pm.label(),
        ));
    }
    Err(anyhow!(
        "unknown task runner {raw:?}; expected one of {}",
        join_labels(TaskRunner::all().iter().copied().map(TaskRunner::label)),
    ))
}

/// Generic CLI-then-env override parser. CLI wins; whitespace is
/// trimmed from both sources before parsing so `RUNNER_PM=" pnpm "`
/// works the same as `RUNNER_PM=pnpm`. Empty/whitespace-only values
/// are treated as unset so a user can clear an inherited variable with
/// `RUNNER_PM= runner …`. Matches the whitespace handling used by
/// [`super::policies::is_env_truthy`] for boolean env flags.
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
