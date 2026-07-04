//! Override construction — `impl ResolutionOverrides` plus the CLI/env
//! parsers that feed it. Policy parsing lives in [`super::policies`];
//! the data shapes live in [`super::types`].

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use super::join_labels;
use super::policies::{
    is_env_truthy, parse_fallback_label, parse_mismatch_label, parse_prefer_runners,
    parse_tasks_overrides, parse_tasks_prefer, resolve_failure_policy, resolve_fallback_policy,
    resolve_mismatch_policy,
};
use super::types::{
    DiagnosticFlags, ExplainSource, OverrideOrigin, OverrideSources, PmOverride,
    ResolutionOverrides, RunnerOverride, ScriptPolicy, SourceValue,
};
use crate::config::{LoadedConfig, parse_node_pm, parse_python_pm};
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
        cli_pm: Option<&str>,
        cli_runner: Option<&str>,
        cli_fallback: Option<&str>,
        cli_on_mismatch: Option<&str>,
        diagnostics: DiagnosticFlags,
        failure: crate::cli::ChainFailureFlags,
        config: Option<&LoadedConfig>,
    ) -> Result<Self> {
        let env = EnvSnapshot::capture();
        let cli = CliSides {
            pm: cli_pm,
            runner: cli_runner,
            fallback: cli_fallback,
            on_mismatch: cli_on_mismatch,
            diagnostics,
            failure,
        };
        Self::from_sources(env.sources(cli, config))
    }

    /// Lenient sibling of [`Self::from_cli_and_env`] for commands that
    /// must keep working when the *environment* is misconfigured —
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
        cli_pm: Option<&str>,
        cli_runner: Option<&str>,
        cli_fallback: Option<&str>,
        cli_on_mismatch: Option<&str>,
        diagnostics: DiagnosticFlags,
        failure: crate::cli::ChainFailureFlags,
        config: Option<&LoadedConfig>,
    ) -> Result<(Self, Vec<DetectionWarning>)> {
        let env = EnvSnapshot::capture();
        let cli = CliSides {
            pm: cli_pm,
            runner: cli_runner,
            fallback: cli_fallback,
            on_mismatch: cli_on_mismatch,
            diagnostics,
            failure,
        };
        Self::from_sources_lenient(env.sources(cli, config))
    }

    /// Pure-function counterpart of [`Self::from_cli_and_env_lenient`]:
    /// pre-validates every env-sourced string field, blanking invalid
    /// values into warnings, then delegates to [`Self::from_sources`].
    ///
    /// Mirrors [`parse_override`] precedence exactly — an env value
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
            &mut sources.install_pms,
            "RUNNER_INSTALL_PMS",
            &mut warnings,
            |raw| {
                raw.split([',', ' ', '\t', '\n'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .try_for_each(|label| parse_pm_label(label).map(drop))
            },
        );
        lenient_env_field(
            &mut sources.install_scripts,
            "RUNNER_INSTALL_SCRIPTS",
            &mut warnings,
            |raw| parse_script_policy_label(raw).map(drop),
        );
        for (field, var) in [
            (&mut sources.no_warnings, "RUNNER_NO_WARNINGS"),
            (&mut sources.quiet, "RUNNER_QUIET"),
            (&mut sources.explain, "RUNNER_EXPLAIN"),
            (&mut sources.keep_going, "RUNNER_KEEP_GOING"),
            (&mut sources.kill_on_fail, "RUNNER_KILL_ON_FAIL"),
        ] {
            lenient_env_bool(field, var, &mut warnings);
        }
        let overrides = Self::from_sources(sources)?;
        Ok((overrides, warnings))
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
        reason = "OverrideSources is a single-use builder; taking by value keeps the call sites \
                  moveable"
    )]
    pub(crate) fn from_sources(sources: OverrideSources<'_>) -> Result<Self> {
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

        let fallback =
            resolve_fallback_policy(sources.fallback.cli, sources.fallback.env, sources.config)?;
        let on_mismatch = resolve_mismatch_policy(
            sources.on_mismatch.cli,
            sources.on_mismatch.env,
            sources.config,
        )?;
        // `[tasks]` (rank-only, PM-aware) supersedes the deprecated
        // `[task_runner].prefer` (restrictive, runners-only). When the new
        // section carries anything, the legacy list is ignored entirely — the
        // config loader has already emitted the deprecation warning.
        //
        // "Carries anything" is judged on the *raw* config fields, not the
        // parsed result: a `[tasks].prefer` entry like `"nx"` is recognized
        // but resolves to no `TaskSource` (see `resolve_source_label`), so
        // checking `prefer_sources.is_empty()` would wrongly treat an
        // explicit-but-source-less `prefer` list as absent and fall through
        // to the legacy, more restrictive list.
        let tasks_section_set = sources.config.is_some_and(|c| {
            !c.config.tasks.prefer.is_empty() || !c.config.tasks.overrides.is_empty()
        });
        let prefer_sources = parse_tasks_prefer(sources.config)?;
        let task_source_overrides = parse_tasks_overrides(sources.config)?;
        let prefer_runners = if tasks_section_set {
            Vec::new()
        } else {
            parse_prefer_runners(sources.config)?
        };
        let no_warnings =
            sources.no_warnings.cli || sources.no_warnings.env.is_some_and(is_env_truthy);
        let quiet = sources.quiet.cli || sources.quiet.env.is_some_and(is_env_truthy);
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
        let install_pms = parse_install_pms(&sources)?;
        let script_policy = parse_install_scripts(&sources)?;

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
            prefer_sources,
            task_source_overrides,
            fallback,
            on_mismatch,
            no_warnings,
            quiet,
            explain,
            failure_policy,
            group_output,
            github_group_parallel,
            parallel_grouped,
            install_pms,
            script_policy,
            // Set by a parent runner that already opened a GHA group (see
            // `crate::cmd::GROUP_ACTIVE_ENV`), captured into `sources` so this
            // stays a pure function of its inputs. An internal nesting signal,
            // not part of the CLI/env/config override layering. Gated through
            // `is_env_truthy` like every other `RUNNER_*` boolean, so
            // `=0`/`=false`/empty read as not-nested (the runner only ever
            // writes `1`). Absent → false.
            parent_group_open: sources.group_active.is_some_and(is_env_truthy),
        })
    }
}

/// Resolve the `runner install` PM allowlist: `RUNNER_INSTALL_PMS` (env,
/// comma/whitespace-separated) wins over `[install].pms` (config). Each
/// entry must name a known package manager; detection (whether the PM is
/// present in *this* project) is checked later in `cmd::install`.
///
/// # Errors
///
/// Returns an error if any entry is not a recognized package manager.
fn parse_install_pms(sources: &OverrideSources<'_>) -> Result<Vec<PackageManager>> {
    if let Some(raw) = sources
        .install_pms
        .env
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return raw
            .split([',', ' ', '\t', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|label| parse_pm_label(label).map_err(|err| anyhow!("RUNNER_INSTALL_PMS: {err}")))
            .collect();
    }
    let Some(loaded) = sources.config else {
        return Ok(Vec::new());
    };
    loaded
        .config
        .install
        .pms
        .iter()
        .map(|label| parse_pm_label(label).map_err(|err| anyhow!("[install].pms: {err}")))
        .collect()
}

/// Resolve the `runner install` lifecycle-script policy: `RUNNER_INSTALL_SCRIPTS`
/// (env) wins over `[install].scripts` (config). The CLI `--no-scripts` /
/// `--scripts` flags are layered on top later, at the dispatch boundary, so they
/// are not consulted here. Unset on both sides yields [`ScriptPolicy::Default`] —
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

/// Parse a single `deny`/`allow` script-policy label (case-sensitive,
/// lowercase-only — matching the sibling enum-label parsers and the
/// committed JSON Schema enum).
///
/// # Errors
///
/// Returns an error naming the (sanitized) value when it is neither `deny`
/// nor `allow`.
fn parse_script_policy_label(raw: &str) -> Result<ScriptPolicy> {
    match raw.trim() {
        "deny" => Ok(ScriptPolicy::Deny),
        "allow" => Ok(ScriptPolicy::Allow),
        _ => Err(anyhow!(
            "unknown script policy \"{}\"; expected \"deny\" or \"allow\"",
            sanitize_raw_label(raw),
        )),
    }
}

/// Validate a loaded `runner.toml` in isolation — no CLI or environment
/// layer — by running it through the real override builder. Every field is
/// parsed exactly as a live dispatch would parse it (PM names, task-runner
/// `prefer` list, `fallback` / `on_mismatch` policies), and the in-file
/// `[chain]` failure-policy conflict (`keep_going` and `kill_on_fail` both
/// `true`) surfaces here too: with no env var to neutralize a side, the
/// same [`ResolveError::ConflictingFailurePolicy`] the resolver raises at
/// dispatch time fires during construction. Delegating keeps `config
/// validate` honest — it can never accept a file a real run would reject.
///
/// # Errors
///
/// Returns the first parse or conflict error in the file.
pub(crate) fn validate_config(loaded: &LoadedConfig) -> Result<()> {
    ResolutionOverrides::from_sources(OverrideSources {
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
    fn install_pms_env_parses_comma_and_space_list() {
        let sources = OverrideSources {
            install_pms: SourceValue {
                cli: None,
                env: Some("bun, cargo deno"),
            },
            ..OverrideSources::default()
        };
        let overrides = ResolutionOverrides::from_sources(sources).expect("env list parses");
        assert_eq!(
            overrides.install_pms,
            vec![
                PackageManager::Bun,
                PackageManager::Cargo,
                PackageManager::Deno
            ]
        );
    }

    #[test]
    fn install_pms_env_rejects_unknown_pm() {
        let sources = OverrideSources {
            install_pms: SourceValue {
                cli: None,
                env: Some("bun,notapm"),
            },
            ..OverrideSources::default()
        };
        let err = ResolutionOverrides::from_sources(sources).expect_err("unknown PM must error");
        assert!(format!("{err:#}").contains("RUNNER_INSTALL_PMS"));
    }

    #[test]
    fn script_policy_defaults_when_unset() {
        let overrides =
            ResolutionOverrides::from_sources(OverrideSources::default()).expect("builds");
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
                ResolutionOverrides::from_sources(sources).expect("script policy parses");
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
        let overrides = ResolutionOverrides::from_sources(sources).expect("env wins over config");
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
        let overrides = ResolutionOverrides::from_sources(sources).expect("config applies");
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
        let err = ResolutionOverrides::from_sources(sources).expect_err("unknown value errors");
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
            let err = ResolutionOverrides::from_sources(sources)
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
        let nested = ResolutionOverrides::from_sources(OverrideSources {
            group_active: Some("1"),
            ..OverrideSources::default()
        })
        .expect("builds");
        assert!(nested.parent_group_open);

        // `0`/empty read as not-nested, matching the other `RUNNER_*` flags.
        for falsy in ["0", "", "false"] {
            let o = ResolutionOverrides::from_sources(OverrideSources {
                group_active: Some(falsy),
                ..OverrideSources::default()
            })
            .expect("builds");
            assert!(!o.parent_group_open, "{falsy:?} should read as not nested");
        }

        // Absent → not nested.
        let absent = ResolutionOverrides::from_sources(OverrideSources::default()).expect("builds");
        assert!(!absent.parent_group_open);
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
    pm: Option<&'a str>,
    runner: Option<&'a str>,
    fallback: Option<&'a str>,
    on_mismatch: Option<&'a str>,
    diagnostics: DiagnosticFlags,
    failure: crate::cli::ChainFailureFlags,
}

/// Captured `RUNNER_*` environment, separated from [`OverrideSources`]
/// assembly so the strict and lenient constructors share one read path
/// and can never drift on which variables they consult.
struct EnvSnapshot {
    pm: Option<String>,
    runner: Option<String>,
    fallback: Option<String>,
    on_mismatch: Option<String>,
    no_warnings: Option<String>,
    quiet: Option<String>,
    explain: Option<String>,
    keep_going: Option<String>,
    kill_on_fail: Option<String>,
    install_pms: Option<String>,
    install_scripts: Option<String>,
    group_active: Option<String>,
}

impl EnvSnapshot {
    /// Read every `RUNNER_*` override variable from the process
    /// environment.
    fn capture() -> Self {
        Self {
            pm: std::env::var("RUNNER_PM").ok(),
            runner: std::env::var("RUNNER_RUNNER").ok(),
            fallback: std::env::var("RUNNER_FALLBACK").ok(),
            on_mismatch: std::env::var("RUNNER_ON_MISMATCH").ok(),
            no_warnings: std::env::var("RUNNER_NO_WARNINGS").ok(),
            quiet: std::env::var("RUNNER_QUIET").ok(),
            explain: std::env::var("RUNNER_EXPLAIN").ok(),
            keep_going: std::env::var("RUNNER_KEEP_GOING").ok(),
            kill_on_fail: std::env::var("RUNNER_KILL_ON_FAIL").ok(),
            install_pms: std::env::var("RUNNER_INSTALL_PMS").ok(),
            install_scripts: std::env::var("RUNNER_INSTALL_SCRIPTS").ok(),
            group_active: std::env::var(crate::cmd::GROUP_ACTIVE_ENV).ok(),
        }
    }

    /// Pair the captured environment with the CLI flag values into the
    /// [`OverrideSources`] consumed by the constructors.
    fn sources<'a>(
        &'a self,
        cli: CliSides<'a>,
        config: Option<&'a LoadedConfig>,
    ) -> OverrideSources<'a> {
        OverrideSources {
            pm: SourceValue {
                cli: cli.pm,
                env: self.pm.as_deref(),
            },
            runner: SourceValue {
                cli: cli.runner,
                env: self.runner.as_deref(),
            },
            fallback: SourceValue {
                cli: cli.fallback,
                env: self.fallback.as_deref(),
            },
            on_mismatch: SourceValue {
                cli: cli.on_mismatch,
                env: self.on_mismatch.as_deref(),
            },
            no_warnings: ExplainSource {
                cli: cli.diagnostics.no_warnings,
                env: self.no_warnings.as_deref(),
            },
            quiet: ExplainSource {
                cli: cli.diagnostics.quiet,
                env: self.quiet.as_deref(),
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
            install_pms: SourceValue {
                cli: None,
                env: self.install_pms.as_deref(),
            },
            install_scripts: SourceValue {
                cli: None,
                env: self.install_scripts.as_deref(),
            },
            group_active: self.group_active.as_deref(),
            config,
        }
    }
}

/// Pre-validate one env-sourced override field for the lenient
/// constructor. The env side is only consulted (and therefore only
/// validated) when the CLI side is unset or whitespace-only — exactly
/// the precedence [`parse_override`] applies — so CLI-shadowed env
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
/// `RUNNER_KEEP_GOING=flase` turned the knob ON — the opposite of the
/// user's clear intent. Recognized (case-insensitive): `1`, `true`,
/// `yes`, `on` / `0`, `false`, `no`, `off`; blank stays "unset" per the
/// resolver-wide convention. A set CLI flag shadows the env value, so it
/// isn't validated (or warned about) then — mirroring
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
    let recognized = matches!(raw, "1" | "0")
        || ["true", "false", "yes", "no", "on", "off"]
            .iter()
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
