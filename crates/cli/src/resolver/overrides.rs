//! Override construction, `impl ResolutionOverrides` plus the CLI/env
//! parsers that feed it. Policy parsing lives in [`super::policies`];
//! the data shapes live in [`super::types`].

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use super::join_labels;
use super::policies::{
    is_env_truthy, parse_collision_label, parse_fallback_label, parse_host_stream_label,
    parse_mismatch_label, parse_quiet_env, parse_reach_label, parse_runtime_label,
    parse_tasks_overrides, parse_tasks_prefer, parse_tasks_verbosity, resolve_failure_policy,
    resolve_fallback_policy, resolve_mismatch_policy,
};
use super::types::{
    CliOverrides, CollisionPolicy, DiagnosticFlags, ExplainSource, LockfilePolicy, OutputGrouping,
    OverrideOrigin, OverrideSources, ParentMarkers, PmOverride, QuietSource, ResolutionOverrides,
    RunnerOverride, RuntimeOverride, ScriptPolicy, SourceValue,
};
use crate::config::{LoadedConfig, parse_node_pm, parse_python_pm};
use crate::tool::{QuietLevel, RunnerOutput, Stream};
use crate::types::{DetectionWarning, Ecosystem, PackageManager, TaskRunner};

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
        overrides: CliOverrides<'_>,
        diagnostics: DiagnosticFlags<'_>,
        failure: crate::args::ChainFailureFlags,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let env = EnvSnapshot::capture();
        let cli = CliSides {
            overrides,
            diagnostics,
            failure,
        };
        let mut built = Self::from_sources(&env.sources(cli, config))?;
        built.package = overrides.package.map(str::to_owned);
        Ok(built)
    }

    /// Lenient sibling of [`Self::from_cli_and_env`] for commands that
    /// must keep working when the *environment* is misconfigured.
    /// `runner doctor` exists to diagnose exactly that, so it can't die
    /// on the condition it should report. Invalid env-sourced override
    /// values are blanked and returned as
    /// [`DetectionWarning::InvalidEnvOverride`]; CLI flag values stay
    /// strict (an explicit flag is an explicit failure).
    ///
    /// # Errors
    ///
    /// Returns an error for everything the strict path rejects except
    /// unparseable env override values: bad CLI values, invalid
    /// `runner.toml` fields, conflicting failure-policy toggles.
    pub(crate) fn from_cli_and_env_lenient(
        overrides: CliOverrides<'_>,
        diagnostics: DiagnosticFlags<'_>,
        failure: crate::args::ChainFailureFlags,
        config: Option<&LoadedConfig>,
    ) -> Result<(Self, Vec<DetectionWarning>)> {
        let env = EnvSnapshot::capture();
        let cli = CliSides {
            overrides,
            diagnostics,
            failure,
        };
        let (mut built, warnings) = Self::from_sources_lenient(env.sources(cli, config))?;
        built.package = overrides.package.map(str::to_owned);
        Ok((built, warnings))
    }

    /// Pure-function counterpart of [`Self::from_cli_and_env_lenient`]:
    /// pre-validates every env-sourced string field, blanking invalid
    /// values into warnings, then delegates to [`Self::from_sources`].
    ///
    /// Mirrors [`parse_override`] precedence exactly: an env value
    /// shadowed by a CLI value is never parsed by the strict path, so
    /// it is not validated (or warned about) here either.
    ///
    /// # Errors
    ///
    /// Same as [`Self::from_cli_and_env_lenient`].
    pub(crate) fn from_sources_lenient(
        mut sources: OverrideSources<'_>,
    ) -> Result<(Self, Vec<DetectionWarning>)> {
        let mut warnings = Vec::new();
        lenient_env_field(&mut sources.pm, "RUNNER_PM", &mut warnings, |raw| {
            parse_pm_label(raw).map(drop)
        });
        lenient_env_field(&mut sources.runner, "RUNNER_RUNNER", &mut warnings, |raw| {
            parse_runner_label(raw).map(drop)
        });
        lenient_env_field(
            &mut sources.runtime,
            "RUNNER_RUNTIME",
            &mut warnings,
            |raw| parse_runtime_label(raw).map(drop),
        );
        lenient_env_field(&mut sources.reach, "RUNNER_REACH", &mut warnings, |raw| {
            parse_reach_label(raw).map(drop)
        });
        lenient_env_field(
            &mut sources.fallback,
            "RUNNER_FALLBACK",
            &mut warnings,
            |raw| parse_fallback_label(raw).map(drop),
        );
        lenient_env_field(
            &mut sources.on_mismatch,
            "RUNNER_ON_MISMATCH",
            &mut warnings,
            |raw| parse_mismatch_label(raw).map(drop),
        );
        lenient_env_field(
            &mut sources.install_scripts,
            "RUNNER_INSTALL_SCRIPTS",
            &mut warnings,
            |raw| parse_script_policy_label(raw).map(drop),
        );
        lenient_env_field(
            &mut sources.install_on_collision,
            "RUNNER_INSTALL_ON_COLLISION",
            &mut warnings,
            |raw| parse_collision_label(raw).map(drop),
        );
        lenient_env_bool(
            &mut sources.no_warnings,
            "RUNNER_NO_WARNINGS",
            &mut warnings,
        );
        // `RUNNER_QUIET` accepts a numeric level (`0..4`, clamped) or a truthy word
        // (level 1), so it validates against `parse_quiet_env` rather than the
        // plain-bool path. A CLI `-q` count shadows the env, mirroring
        // `lenient_env_field`. Ordered here (between no-warnings and explain)
        // to keep warning emission in the historical env-field order.
        if sources.quiet.cli == 0
            && let Some(raw) = sources.quiet.env.map(str::trim).filter(|s| !s.is_empty())
            && parse_quiet_env(raw).is_none()
        {
            let sanitized = sanitize_raw_label(raw);
            warnings.push(DetectionWarning::InvalidEnvOverride {
                var: "RUNNER_QUIET",
                raw: sanitized.clone(),
                message: sanitize_error_message(
                    raw,
                    &sanitized,
                    "expected a level 0-4 or a boolean (1/true/yes/on)",
                ),
            });
            sources.quiet.env = None;
        }
        lenient_env_field(
            &mut sources.host_stream,
            "RUNNER_HOST_STREAM",
            &mut warnings,
            |raw| parse_host_stream_label(raw).map(drop),
        );
        for (field, var) in [
            (&mut sources.explain, "RUNNER_EXPLAIN"),
            (&mut sources.keep_going, "RUNNER_KEEP_GOING"),
            (&mut sources.kill_on_fail, "RUNNER_KILL_ON_FAIL"),
        ] {
            lenient_env_bool(field, var, &mut warnings);
        }
        let overrides = Self::from_sources(&sources)?;
        Ok((overrides, warnings))
    }

    /// Pure-function constructor over a fully-populated
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
    pub(crate) fn from_sources(sources: &OverrideSources<'_>) -> Result<Self> {
        let pm = parse_override(
            sources.pm.cli,
            sources.pm.env,
            &PM_SOURCE_NAMES,
            parse_pm_label,
            |pm, origin| PmOverride { pm, origin },
        )?;
        let runner = parse_override(
            sources.runner.cli,
            sources.runner.env,
            &RUNNER_SOURCE_NAMES,
            parse_runner_label,
            |runner, origin| RunnerOverride { runner, origin },
        )?;

        let runtime = resolve_runtime(sources)?;
        let reach = resolve_reach(sources)?;
        let lockfile = resolve_lockfile(sources);
        let fallback =
            resolve_fallback_policy(sources.fallback.cli, sources.fallback.env, sources.config)?;
        let on_mismatch = resolve_mismatch_policy(
            sources.on_mismatch.cli,
            sources.on_mismatch.env,
            sources.config,
        )?;
        let prefer_sources = parse_tasks_prefer(sources.config)?;
        let task_source_overrides = parse_tasks_overrides(sources.config)?;
        let task_verbosity = parse_tasks_verbosity(sources.config)?;
        let prefer_runners = Vec::new();
        let no_warnings =
            sources.no_warnings.cli || sources.no_warnings.env.is_some_and(is_env_truthy);
        let (quiet_level, host_stream) = resolve_verbosity(sources)?;
        let (output_policy, host_diagnostics_explicit) =
            resolve_output_policy(sources, quiet_level, no_warnings)?;
        let host_stream_config = sources
            .config
            .and_then(|config| config.config.host.stream.as_deref())
            .map(parse_host_stream_label)
            .transpose()?
            .unwrap_or(Stream::Inherit);
        let explain = sources.explain.cli || sources.explain.env.is_some_and(is_env_truthy);
        let failure_policy =
            resolve_failure_policy(sources.keep_going, sources.kill_on_fail, sources.config)?;
        let grouping = resolve_grouping(sources);
        let script_policy = parse_install_scripts(sources)?;
        let on_collision = parse_install_on_collision(sources)?;
        let pm_by_ecosystem = config_pm_by_ecosystem(sources)?;

        Ok(Self {
            pm,
            reach,
            lockfile,
            package: None,
            pm_by_ecosystem,
            runner,
            runtime,
            prefer_runners,
            prefer_sources,
            task_source_overrides,
            fallback,
            on_mismatch,
            no_warnings,
            quiet_level,
            host_diagnostics_explicit,
            output_policy,
            host_stream,
            host_stream_config,
            task_verbosity,
            explain,
            failure_policy,
            grouping,
            script_policy,
            on_collision,
            env: env_layers(sources),
            tool_install: tool_install(sources),
            // `warned` is set in `dispatch`, the first place a resolved
            // project root and the inherited `RUNNER_WARNED_ROOT` marker are
            // both in hand. `group_open` is gated through `is_env_truthy` like
            // every other `RUNNER_*` boolean, so `=0`/`=false`/empty read as
            // not-nested.
            parent: ParentMarkers {
                group_open: sources.group_active.is_some_and(is_env_truthy),
                warned: false,
            },
        })
    }
}

/// `--fetch` / `RUNNER_REACH` first, then `[defaults].fetch`.
fn resolve_reach(sources: &OverrideSources<'_>) -> Result<runner_core::ReachPolicy> {
    Ok(sources
        .reach
        .cli
        .or(sources.reach.env)
        .or_else(|| {
            sources
                .config
                .and_then(|loaded| loaded.config.defaults.fetch.as_deref())
        })
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(parse_reach_label)
        .transpose()?
        .unwrap_or_default())
}

fn resolve_lockfile(sources: &OverrideSources<'_>) -> LockfilePolicy {
    if sources
        .config
        .and_then(|loaded| loaded.config.defaults.frozen)
        .unwrap_or(false)
    {
        LockfilePolicy::Frozen
    } else {
        LockfilePolicy::Update
    }
}

/// The output policy after `[runner]`/`[host]` config and an explicit quiet
/// preset, plus whether the host diagnostic axis was chosen explicitly.
fn resolve_output_policy(
    sources: &OverrideSources<'_>,
    quiet_level: QuietLevel,
    no_warnings: bool,
) -> Result<(crate::tool::OutputPolicy, bool)> {
    let quiet_explicit =
        sources.quiet.cli > 0 || sources.quiet.env.and_then(parse_quiet_env).is_some();
    let host_diagnostics_explicit = quiet_explicit
        || sources
            .config
            .is_some_and(|config| config.config.host.diagnostics.is_some());
    let mut output_policy = crate::tool::OutputPolicy::default();
    if let Some(config) = sources.config {
        for output in RunnerOutput::ALL {
            if let Some(shown) = config.config.runner.get(output) {
                output_policy.runner = output_policy.runner.with(output, shown);
            }
        }
        if let Some(raw) = config.config.host.diagnostics.as_deref() {
            output_policy.host_diagnostics = crate::tool::HostDiagnostics::from_label(raw)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "[host] diagnostics {raw:?}; expected one of normal, quiet, reduced"
                    )
                })?;
        }
    }
    if quiet_explicit {
        let preset = crate::tool::OutputPolicy::from_quiet(quiet_level);
        output_policy.runner = output_policy.runner.and(preset.runner);
        output_policy.host_diagnostics =
            output_policy.host_diagnostics.max(preset.host_diagnostics);
    }
    if no_warnings {
        output_policy.runner = output_policy.runner.with(RunnerOutput::Warnings, false);
    }
    Ok((output_policy, host_diagnostics_explicit))
}

/// Output grouping toggles (no CLI/env layer in v1). `group_output`
/// (default true) is the broad GitHub Actions grouping switch.
/// `github_group_parallel` (default true) applies under Actions only when
/// `group_output` is also true; `parallel_grouped` (default false) applies
/// elsewhere.
fn resolve_grouping(sources: &OverrideSources<'_>) -> OutputGrouping {
    OutputGrouping {
        group_output: sources.config.is_none_or(|c| c.config.github.group_output),
        github_group_parallel: sources
            .config
            .is_none_or(|c| c.config.github.group_parallel),
        parallel_grouped: sources.config.is_some_and(|c| c.config.parallel.grouped),
    }
}

/// `[pm].node` and `[pm].python`.
fn config_pm_by_ecosystem(sources: &OverrideSources<'_>) -> Result<HashMap<Ecosystem, PmOverride>> {
    let mut pm_by_ecosystem = HashMap::new();
    let Some(loaded) = sources.config else {
        return Ok(pm_by_ecosystem);
    };
    let origin = || OverrideOrigin::ConfigFile {
        path: loaded.path.clone(),
    };
    if let Some(raw) = loaded.config.pm.node.as_deref() {
        pm_by_ecosystem.insert(
            Ecosystem::Node,
            PmOverride {
                pm: parse_node_pm(raw)?,
                origin: origin(),
            },
        );
    }
    if let Some(raw) = loaded.config.pm.python.as_deref() {
        pm_by_ecosystem.insert(
            Ecosystem::Python,
            PmOverride {
                pm: parse_python_pm(raw)?,
                origin: origin(),
            },
        );
    }
    Ok(pm_by_ecosystem)
}

/// Resolve the JS-runtime override: `--runtime` / `RUNNER_RUNTIME` first,
/// then `[runtime].js`. The config layer is folded in here rather than in
/// [`parse_override`], which only knows the CLI and env sides.
fn resolve_runtime(sources: &OverrideSources<'_>) -> Result<Option<RuntimeOverride>> {
    parse_override(
        sources.runtime.cli,
        sources.runtime.env,
        &RUNTIME_SOURCE_NAMES,
        parse_runtime_label,
        |runtime, origin| RuntimeOverride { runtime, origin },
    )?
    .map_or_else(
        || resolve_config_runtime(sources.config),
        |over| Ok(Some(over)),
    )
}

/// `[runtime].js`, the config layer of the runtime override. Returns `None`
/// when no config is loaded or the key is absent.
fn resolve_config_runtime(config: Option<&LoadedConfig>) -> Result<Option<RuntimeOverride>> {
    let Some(loaded) = config else {
        return Ok(None);
    };
    let Some(raw) = loaded.config.runtime.js.as_deref() else {
        return Ok(None);
    };
    Ok(Some(RuntimeOverride {
        runtime: parse_runtime_label(raw)?,
        origin: OverrideOrigin::ConfigFile {
            path: loaded.path.clone(),
        },
    }))
}

/// Resolve the `runner install` lifecycle-script policy: `RUNNER_INSTALL_SCRIPTS`
/// (env) wins over `[install].scripts` (config). The CLI `--no-scripts` /
/// `--scripts` flags are layered on top later, at the dispatch boundary, so they
/// are not consulted here. Unset on both sides yields [`ScriptPolicy::Default`]:
/// each package manager keeps its own default.
///
/// # Errors
///
/// Returns an error if either source holds a value that is not `deny` or `allow`.
fn parse_install_scripts(sources: &OverrideSources<'_>) -> Result<ScriptPolicy> {
    if let Some(raw) = sources
        .install_scripts
        .env
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return parse_script_policy_label(raw)
            .map_err(|err| anyhow!("RUNNER_INSTALL_SCRIPTS: {err}"));
    }
    if let Some(raw) = sources
        .config
        .and_then(|loaded| loaded.config.install.scripts.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return parse_script_policy_label(raw).map_err(|err| anyhow!("[install].scripts: {err}"));
    }
    Ok(ScriptPolicy::Default)
}

/// `RUNNER_INSTALL_ON_COLLISION` (env) → `[install].on_collision` (config),
/// highest first. Absent leaves [`CollisionPolicy::Resolve`].
fn parse_install_on_collision(sources: &OverrideSources<'_>) -> Result<CollisionPolicy> {
    if let Some(raw) = sources
        .install_on_collision
        .env
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return parse_collision_label(raw)
            .map_err(|err| anyhow!("RUNNER_INSTALL_ON_COLLISION: {err}"));
    }
    if let Some(raw) = sources
        .config
        .and_then(|loaded| loaded.config.install.on_collision.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return parse_collision_label(raw).map_err(|err| anyhow!("[install].on_collision: {err}"));
    }
    Ok(CollisionPolicy::default())
}

/// Collect `[env]`, `[tools.*].env` and `[tasks.*].env` into the three
/// layers a spawn merges. Config-only: an environment variable layer set from
/// the environment would be the environment already.
fn env_layers(sources: &OverrideSources<'_>) -> super::types::EnvLayers {
    let Some(loaded) = sources.config else {
        return super::types::EnvLayers::default();
    };
    super::types::EnvLayers {
        project: loaded.config.env.clone(),
        tool: loaded
            .config
            .tools
            .iter()
            .filter(|(_, settings)| !settings.env.is_empty())
            .map(|(name, settings)| (name.clone(), settings.env.clone()))
            .collect(),
        task: loaded
            .config
            .tasks
            .tasks
            .iter()
            .filter_map(|(name, spec)| match spec {
                crate::config::TaskSpec::Settings(settings) if !settings.env.is_empty() => {
                    Some((name.clone(), settings.env.clone()))
                }
                _ => None,
            })
            .collect(),
    }
}

/// `[tools.<name>].install`, normalized to an ordered operation list per tool.
fn tool_install(sources: &OverrideSources<'_>) -> std::collections::BTreeMap<String, Vec<String>> {
    sources
        .config
        .map_or_else(std::collections::BTreeMap::new, |loaded| {
            loaded
                .config
                .tools
                .iter()
                .filter_map(|(name, settings)| {
                    settings
                        .install
                        .as_ref()
                        .map(|run| (name.clone(), run.operations()))
                })
                .collect()
        })
}

/// Parse a single `deny`/`allow` script-policy label (case-sensitive,
/// lowercase-only, matching the sibling enum-label parsers and the
/// committed JSON Schema enum).
///
/// # Errors
///
/// Returns an error naming the (sanitized) value when it is neither `deny`
/// nor `allow`.
fn parse_script_policy_label(raw: &str) -> Result<ScriptPolicy> {
    let trimmed = raw.trim();
    ScriptPolicy::SETTABLE
        .into_iter()
        .find(|policy| policy.label() == Some(trimmed))
        .ok_or_else(|| {
            anyhow!(
                "unknown script policy \"{}\"; expected \"{}\"",
                sanitize_raw_label(raw),
                ScriptPolicy::SETTABLE
                    .iter()
                    .filter_map(|p| p.label())
                    .collect::<Vec<_>>()
                    .join("\" or \""),
            )
        })
}

/// Validate a loaded `runner.toml` in isolation, no CLI or environment
/// layer, by running it through the real override builder. Every field is
/// parsed exactly as a live dispatch would parse it (PM names, task-runner
/// `prefer` list, `fallback` / `on_mismatch` policies), and the in-file
/// `[chain]` failure-policy conflict (`keep_going` and `kill_on_fail` both
/// `true`) surfaces here too: with no env var to neutralize a side, the
/// same [`ResolveError::ConflictingFailurePolicy`] the resolver raises at
/// dispatch time fires during construction. Delegating keeps `config
/// validate` honest: it can never accept a file a real run would reject.
///
/// # Errors
///
/// Returns the first parse or conflict error in the file.
pub(crate) fn validate_config(loaded: &LoadedConfig) -> Result<()> {
    ResolutionOverrides::from_sources(&OverrideSources {
        config: Some(loaded),
        ..OverrideSources::default()
    })
    .map(drop)
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
        "unknown package manager \"{}\"; expected one of {}",
        sanitize_raw_label(raw),
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
        "unknown task runner \"{}\"; expected one of {}",
        sanitize_raw_label(raw),
        join_labels(TaskRunner::all().iter().copied().map(TaskRunner::label)),
    ))
}

/// Maximum characters of a raw override value rendered in an error.
const MAX_RAW_DISPLAY: usize = 60;

/// Render an untrusted override value safely for a one-line error:
/// control characters (ANSI escapes, newlines) are escaped via
/// [`char::escape_debug`], then truncated to [`MAX_RAW_DISPLAY`] chars.
/// Env values can be arbitrary captured command output (e.g. a
/// PowerShell REPL banner from an unquoted assignment), hence both.
fn sanitize_raw_label(raw: &str) -> String {
    let escaped: String = raw.chars().flat_map(char::escape_debug).collect();
    let mut chars = escaped.chars();
    let truncated: String = chars.by_ref().take(MAX_RAW_DISPLAY).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{InstallSection, RunnerConfig};

    #[test]
    fn install_pms_env_has_no_declared_row() {
        assert!(runner_core::Setting::by_env("RUNNER_INSTALL_PMS").is_none());
    }

    #[test]
    fn script_policy_defaults_when_unset() {
        let overrides =
            ResolutionOverrides::from_sources(&OverrideSources::default()).expect("builds");
        assert_eq!(overrides.script_policy, ScriptPolicy::Default);
    }

    #[test]
    fn script_policy_env_parses_deny_and_allow() {
        for (raw, expected) in [
            ("deny", ScriptPolicy::Deny),
            ("allow", ScriptPolicy::Allow),
            (" deny ", ScriptPolicy::Deny),
        ] {
            let sources = OverrideSources {
                install_scripts: SourceValue {
                    cli: None,
                    env: Some(raw),
                },
                ..OverrideSources::default()
            };
            let overrides =
                ResolutionOverrides::from_sources(&sources).expect("script policy parses");
            assert_eq!(overrides.script_policy, expected, "raw: {raw:?}");
        }
    }

    #[test]
    fn script_policy_env_overrides_config() {
        let loaded = LoadedConfig {
            path: std::path::PathBuf::from("/tmp/runner.toml"),
            config: RunnerConfig {
                install: InstallSection {
                    scripts: Some("allow".to_string()),
                    ..InstallSection::default()
                },
                ..RunnerConfig::default()
            },
            warnings: Vec::new(),
        };
        let sources = OverrideSources {
            install_scripts: SourceValue {
                cli: None,
                env: Some("deny"),
            },
            config: Some(&loaded),
            ..OverrideSources::default()
        };
        let overrides = ResolutionOverrides::from_sources(&sources).expect("env wins over config");
        assert_eq!(overrides.script_policy, ScriptPolicy::Deny);
    }

    #[test]
    fn script_policy_config_applies_when_env_absent() {
        let loaded = LoadedConfig {
            path: std::path::PathBuf::from("/tmp/runner.toml"),
            config: RunnerConfig {
                install: InstallSection {
                    scripts: Some("deny".to_string()),
                    ..InstallSection::default()
                },
                ..RunnerConfig::default()
            },
            warnings: Vec::new(),
        };
        let sources = OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        };
        let overrides = ResolutionOverrides::from_sources(&sources).expect("config applies");
        assert_eq!(overrides.script_policy, ScriptPolicy::Deny);
    }

    #[test]
    fn script_policy_env_rejects_unknown_value() {
        let sources = OverrideSources {
            install_scripts: SourceValue {
                cli: None,
                env: Some("skip"),
            },
            ..OverrideSources::default()
        };
        let err = ResolutionOverrides::from_sources(&sources).expect_err("unknown value errors");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("RUNNER_INSTALL_SCRIPTS"),
            "names the source: {msg}"
        );
        assert!(msg.contains("deny"), "lists valid values: {msg}");
    }

    #[test]
    fn script_policy_env_rejects_case_variants() {
        // Lowercase-only, matching the sibling enum-label parsers and the
        // committed JSON Schema enum (`["deny", "allow", null]`).
        for raw in ["Deny", "ALLOW", "Allow", "DENY"] {
            let sources = OverrideSources {
                install_scripts: SourceValue {
                    cli: None,
                    env: Some(raw),
                },
                ..OverrideSources::default()
            };
            let err = ResolutionOverrides::from_sources(&sources)
                .expect_err("case variants must be rejected");
            assert!(
                format!("{err:#}").contains("unknown script policy"),
                "rejects {raw:?}",
            );
        }
    }

    #[test]
    fn script_policy_lenient_env_garbage_degrades_to_warning() {
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            install_scripts: SourceValue {
                cli: None,
                env: Some("nonsense"),
            },
            ..OverrideSources::default()
        })
        .expect("lenient pass absorbs script-policy env garbage");
        assert_eq!(overrides.script_policy, ScriptPolicy::Default);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn group_active_marker_sets_parent_group_open_truthily() {
        // Threaded through captured sources (no process-env read), so this is
        // testable and `from_sources` stays pure. `1` → nested.
        let nested = ResolutionOverrides::from_sources(&OverrideSources {
            group_active: Some("1"),
            ..OverrideSources::default()
        })
        .expect("builds");
        assert!(nested.parent.group_open);

        // `0`/empty read as not-nested, matching the other `RUNNER_*` flags.
        for falsy in ["0", "", "false"] {
            let o = ResolutionOverrides::from_sources(&OverrideSources {
                group_active: Some(falsy),
                ..OverrideSources::default()
            })
            .expect("builds");
            assert!(!o.parent.group_open, "{falsy:?} should read as not nested");
        }

        // Absent → not nested.
        let absent =
            ResolutionOverrides::from_sources(&OverrideSources::default()).expect("builds");
        assert!(!absent.parent.group_open);
    }

    #[test]
    fn lenient_policy_env_garbage_does_not_leak_full_raw_value() {
        let token_prefix = "ghp_";
        let fake_token = format!(
            "{token_prefix}{}DO_NOT_LEAK_ME",
            "A".repeat(MAX_RAW_DISPLAY.saturating_sub(token_prefix.len()))
        );
        let huge = fake_token.repeat(6);
        let (_overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            fallback: SourceValue {
                cli: None,
                env: Some(&huge),
            },
            ..OverrideSources::default()
        })
        .expect("lenient pass must absorb fallback env garbage");

        assert_eq!(warnings.len(), 1);
        let detail = warnings[0].detail();
        assert!(
            detail.contains('…'),
            "long invalid env value should be truncated in warning detail"
        );
        assert!(
            !detail.contains("DO_NOT_LEAK_ME"),
            "secret-looking env tail must not leak in warning detail"
        );
    }
}

/// The CLI-flag half of an override assembly, bundled so
/// [`EnvSnapshot::sources`] pairs one CLI side with one env snapshot
/// instead of threading seven loose parameters.
#[derive(Clone, Copy)]
struct CliSides<'a> {
    overrides: CliOverrides<'a>,
    diagnostics: DiagnosticFlags<'a>,
    failure: crate::args::ChainFailureFlags,
}

/// Declare the captured `RUNNER_*` environment from one row per variable.
///
/// The field and the variable it reads used to be two declarations that
/// agreed only by hand.
macro_rules! env_snapshot {
    ($($(#[$meta:meta])* $field:ident => $var:expr),* $(,)?) => {
        /// Captured `RUNNER_*` environment, separated from [`OverrideSources`]
        /// assembly so the strict and lenient constructors share one read path
        /// and can never drift on which variables they consult.
        struct EnvSnapshot {
            $($(#[$meta])* $field: Option<String>,)*
        }

        impl EnvSnapshot {
            /// Read every `RUNNER_*` override variable from the process
            /// environment.
            fn capture() -> Self {
                Self { $($field: std::env::var($var).ok(),)* }
            }
        }
    };
}

env_snapshot! {
    pm => runner_core::Setting::env_for("pm"),
    runner => runner_core::Setting::env_for("tasks.prefer"),
    runtime => runner_core::Setting::env_for("runtime.js"),
    reach => runner_core::Setting::env_for("defaults.fetch"),
    fallback => "RUNNER_FALLBACK",
    on_mismatch => "RUNNER_ON_MISMATCH",
    no_warnings => "RUNNER_NO_WARNINGS",
    quiet => runner_core::Setting::env_for("defaults.verbosity"),
    host_stream => "RUNNER_HOST_STREAM",
    explain => "RUNNER_EXPLAIN",
    keep_going => "RUNNER_KEEP_GOING",
    kill_on_fail => "RUNNER_KILL_ON_FAIL",
    install_scripts => runner_core::Setting::env_for("install.scripts"),
    install_on_collision => "RUNNER_INSTALL_ON_COLLISION",
    group_active => crate::commands::GROUP_ACTIVE_ENV,
}

impl EnvSnapshot {
    /// Pair the captured environment with the CLI flag values into the
    /// [`OverrideSources`] consumed by the constructors.
    fn sources<'a>(
        &'a self,
        cli: CliSides<'a>,
        config: Option<&'a LoadedConfig>,
    ) -> OverrideSources<'a> {
        OverrideSources {
            pm: SourceValue {
                cli: cli.overrides.pm,
                env: self.pm.as_deref(),
            },
            runner: SourceValue {
                cli: cli.overrides.runner,
                env: self.runner.as_deref(),
            },
            runtime: SourceValue {
                cli: cli.overrides.runtime,
                env: self.runtime.as_deref(),
            },
            reach: SourceValue {
                cli: cli.overrides.reach,
                env: self.reach.as_deref(),
            },
            fallback: SourceValue {
                cli: cli.overrides.fallback,
                env: self.fallback.as_deref(),
            },
            on_mismatch: SourceValue {
                cli: cli.overrides.on_mismatch,
                env: self.on_mismatch.as_deref(),
            },
            no_warnings: ExplainSource {
                cli: cli.diagnostics.no_warnings,
                env: self.no_warnings.as_deref(),
            },
            quiet: QuietSource {
                cli: cli.diagnostics.quiet,
                env: self.quiet.as_deref(),
            },
            host_stream: SourceValue {
                cli: cli.diagnostics.host_stream,
                env: self.host_stream.as_deref(),
            },
            explain: ExplainSource {
                cli: cli.diagnostics.explain,
                env: self.explain.as_deref(),
            },
            keep_going: ExplainSource {
                cli: cli.failure.keep_going,
                env: self.keep_going.as_deref(),
            },
            kill_on_fail: ExplainSource {
                cli: cli.failure.kill_on_fail,
                env: self.kill_on_fail.as_deref(),
            },
            install_scripts: SourceValue {
                cli: None,
                env: self.install_scripts.as_deref(),
            },
            install_on_collision: SourceValue {
                cli: None,
                env: self.install_on_collision.as_deref(),
            },
            group_active: self.group_active.as_deref(),
            config,
        }
    }
}

/// Resolve the two global verbosity axes from CLI + env.
///
/// Quiet level follows the resolver-wide **CLI > env** precedence: the CLI
/// repeat count (`-q`/`-qq`/`-qqq`) wins whenever the flag was passed
/// (`cli > 0`), else the env value (`RUNNER_QUIET` numeric `0..4`, clamped, or a truthy
/// word → level 1) applies. On the old `{off, on}` bool this is identical to
/// `cli || env` (a set flag was already the ceiling); unlike a `max`, env can
/// no longer escalate a passed `-q` up to `Silent`. Stream takes CLI
/// `--host-stream` first, then `RUNNER_HOST_STREAM`; an unrecognized env value
/// falls back to the default — the same leniency the quiet axis gives bad env —
/// rather than aborting the run (the doctor/lenient path warns instead). A
/// bad explicit `--host-stream` still errors. Per-task
/// `[tasks.<name>].verbosity` config layers under both at dispatch.
fn resolve_verbosity(sources: &OverrideSources<'_>) -> Result<(QuietLevel, Option<Stream>)> {
    // CLI count wins outright when passed, so env can neither escalate past nor
    // undercut an explicit `-q`; env applies only when no `-q` was given.
    let quiet_level = if sources.quiet.cli > 0 {
        QuietLevel::from_count(sources.quiet.cli)
    } else {
        match sources.quiet.env {
            Some(raw) => parse_quiet_env(raw).ok_or_else(|| {
                anyhow!("RUNNER_QUIET={raw}: expected a level 0 to 4 or a true/false word")
            })?,
            None => QuietLevel::Off,
        }
    };

    let cli_host_stream = sources
        .host_stream
        .cli
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let env_host_stream = sources
        .host_stream
        .env
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let host_stream = match (cli_host_stream, env_host_stream) {
        (Some(raw), _) => Some(parse_host_stream_label(raw)?),
        (None, Some(raw)) => Some(
            parse_host_stream_label(raw)
                .map_err(|error| anyhow!("RUNNER_HOST_STREAM={raw}: {error}"))?,
        ),
        (None, None) => None,
    };
    Ok((quiet_level, host_stream))
}

/// Pre-validate one env-sourced override field for the lenient
/// constructor. The env side is only consulted (and therefore only
/// validated) when the CLI side is unset or whitespace-only, exactly
/// the precedence [`parse_override`] applies, so CLI-shadowed env
/// garbage stays invisible, same as the strict path. An invalid env
/// value is blanked from `field` and reported as a warning carrying
/// the sanitized value and the bare parse error.
fn lenient_env_field(
    field: &mut SourceValue<'_>,
    var: &'static str,
    warnings: &mut Vec<DetectionWarning>,
    validate: impl Fn(&str) -> Result<()>,
) {
    if field.cli.map(str::trim).is_some_and(|s| !s.is_empty()) {
        return;
    }
    let Some(raw) = field.env.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    if let Err(err) = validate(raw) {
        let sanitized = sanitize_raw_label(raw);
        warnings.push(DetectionWarning::InvalidEnvOverride {
            var,
            raw: sanitized.clone(),
            message: sanitize_error_message(raw, &sanitized, &format!("{err}")),
        });
        field.env = None;
    }
}

/// Boolean counterpart of [`lenient_env_field`]: a `RUNNER_*` toggle
/// whose value is not a recognized boolean token warns and is ignored
/// instead of silently reading as truthy. Without this, a typo like
/// `RUNNER_KEEP_GOING=flase` turned the knob ON, the opposite of the
/// user's clear intent. Recognized (case-insensitive): `1`, `true`,
/// `yes`, `on` / `0`, `false`, `no`, `off`; blank stays "unset" per the
/// resolver-wide convention. A set CLI flag shadows the env value, so it
/// isn't validated (or warned about) then, mirroring
/// [`lenient_env_field`].
fn lenient_env_bool(
    field: &mut ExplainSource<'_>,
    var: &'static str,
    warnings: &mut Vec<DetectionWarning>,
) {
    if field.cli {
        return;
    }
    let Some(raw) = field.env.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    let recognized = super::policies::ENV_BOOL_TRUTHY
        .iter()
        .chain(super::policies::ENV_BOOL_FALSY)
        .any(|token| raw.eq_ignore_ascii_case(token));
    if !recognized {
        warnings.push(DetectionWarning::InvalidEnvOverride {
            var,
            raw: sanitize_raw_label(raw),
            message: "expected a boolean: 1|true|yes|on or 0|false|no|off".to_string(),
        });
        field.env = None;
    }
}

fn sanitize_error_message(raw: &str, sanitized: &str, message: &str) -> String {
    let escaped: String = raw.chars().flat_map(char::escape_debug).collect();
    message.replace(raw, sanitized).replace(&escaped, sanitized)
}

/// Source names for the cross-ecosystem PM override.
const PM_SOURCE_NAMES: SourceNames = SourceNames {
    cli: "--pm",
    env: "RUNNER_PM",
    example: "pnpm",
};

/// Source names for the task-runner override.
const RUNNER_SOURCE_NAMES: SourceNames = SourceNames {
    cli: "--runner",
    env: "RUNNER_RUNNER",
    example: "just",
};

/// Source names for the JS-runtime override.
const RUNTIME_SOURCE_NAMES: SourceNames = SourceNames {
    cli: "--runtime",
    env: "RUNNER_RUNTIME",
    example: "bun",
};

/// The user-facing names of one override's sources, used to attribute
/// parse errors to the flag or variable that carried the bad value.
struct SourceNames {
    /// CLI flag, e.g. `--pm`.
    cli: &'static str,
    /// Environment variable, e.g. `RUNNER_PM`.
    env: &'static str,
    /// A valid example value, e.g. `pnpm`.
    example: &'static str,
}

impl SourceNames {
    /// Prefix `err` with the source that supplied `raw`. Line breaks
    /// signal captured command output rather than a typed name, so
    /// append a hint showing the correct spelling for that source.
    fn decorate(&self, err: &anyhow::Error, raw: &str, origin: &OverrideOrigin) -> anyhow::Error {
        let from_env = matches!(origin, OverrideOrigin::EnvVar);
        let source = if from_env { self.env } else { self.cli };
        let hint = if raw.contains('\n') || raw.contains('\r') {
            let example = if from_env {
                format!(
                    "$env:{}='{}' (quote the value in PowerShell)",
                    self.env, self.example
                )
            } else {
                format!("{} {}", self.cli, self.example)
            };
            format!(
                "\n  hint: the value contains line breaks and looks like captured command output; \
                 pass a plain name instead, e.g. {example}"
            )
        } else {
            String::new()
        };
        anyhow!("{source}: {err}{hint}")
    }
}

/// Generic CLI-then-env override parser. CLI wins; whitespace is
/// trimmed from both sources before parsing so `RUNNER_PM=" pnpm "`
/// works the same as `RUNNER_PM=pnpm`. Empty/whitespace-only values
/// are treated as unset so a user can clear an inherited variable with
/// `RUNNER_PM= runner …`. Matches the whitespace handling used by
/// [`super::policies::is_env_truthy`] for boolean env flags.
///
/// Parse failures are attributed to the source that carried the value
/// (`names.cli` or `names.env`) via [`SourceNames::decorate`].
fn parse_override<T, P, V, B>(
    cli: Option<&str>,
    env: Option<&str>,
    names: &SourceNames,
    parse: V,
    build: B,
) -> Result<Option<T>>
where
    V: Fn(&str) -> Result<P>,
    B: Fn(P, OverrideOrigin) -> T,
{
    if let Some(raw) = cli.map(str::trim).filter(|s| !s.is_empty()) {
        let parsed =
            parse(raw).map_err(|err| names.decorate(&err, raw, &OverrideOrigin::CliFlag))?;
        return Ok(Some(build(parsed, OverrideOrigin::CliFlag)));
    }
    if let Some(raw) = env.map(str::trim).filter(|s| !s.is_empty()) {
        let parsed =
            parse(raw).map_err(|err| names.decorate(&err, raw, &OverrideOrigin::EnvVar))?;
        return Ok(Some(build(parsed, OverrideOrigin::EnvVar)));
    }
    Ok(None)
}
