//! The override chain for one invocation: CLI flags, `RUNNER_*` variables
//! and `runner.toml`, parsed once into [`ResolutionOverrides`].
//!
//! # Module layout
//!
//! - [`types`], data shapes ([`ResolutionOverrides`], the policy enums,
//!   override-builder helpers).
//! - [`overrides`], `impl ResolutionOverrides` and the CLI/env parsers
//!   that feed it.
//! - [`policies`], pure string→enum/bool parsing for the `FallbackPolicy`,
//!   `MismatchPolicy`, and `FailurePolicy` knobs.
//! - [`error`], the `ResolveError` type surfaced to callers.
//! - [`probe`], `$PATH` probing shared with the doctor.

mod error;
mod overrides;
mod policies;
pub(crate) mod probe;
mod types;

pub(crate) use error::ResolveError;
/// Re-export of the standalone `runner.toml` validator backing
/// `commands::config::validate`; see [`overrides::validate_config`].
pub(crate) use overrides::validate_config;
pub(crate) use policies::parse_quiet_env;
/// Re-export of the canonical Node PATH-probe order so the doctor's
/// schema layer doesn't carry its own copy.
pub(crate) use probe::node_probe_order;
/// Re-export of the pure-function probe variant for the `doctor` subcommand.
/// Lets `commands::doctor` exercise the same PATH walk the resolver uses without
/// owning the env-reading logic.
pub(crate) use probe::probe_in as probe_path_for_doctor;
/// Re-exported for unit tests that need to construct override state
/// directly (e.g. `commands::install::tests`); production code receives
/// overrides fully built by [`ResolutionOverrides::from_cli_and_env`].
#[cfg(test)]
pub(crate) use types::PmOverride;
pub(crate) use types::{
    CliOverrides, CollisionPolicy, DiagnosticFlags, FallbackPolicy, LockfilePolicy, MismatchPolicy,
    OutputGrouping, OverrideOrigin, OverrideSources, ResolutionOverrides, ScriptPolicy,
};
#[cfg(test)]
pub(crate) use types::{RunnerOverride, RuntimeOverride};

/// Join an iterator of `&'static str` labels with `", "`. Used by the
/// override and policy parsers to format `"unknown X; expected one of ..."`
/// diagnostics. Free function rather than a method on a wrapper type
/// because both [`overrides`] and [`policies`] reach for it without
/// sharing other code.
pub(super) fn join_labels<I: Iterator<Item = &'static str>>(labels: I) -> String {
    labels.collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::types::{
        ExplainSource, OverrideSources, QuietSource, RunnerOverride, SourceValue, TaskVerbosity,
    };
    use super::{OverrideOrigin, ResolutionOverrides, ResolveError};
    use crate::config::{LoadedConfig, PmSection, RunnerConfig};
    use crate::tool::test_support::TempDir;
    use crate::types::{DetectionWarning, Ecosystem, PackageManager, TaskRunner};

    #[test]
    fn cli_pm_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("--pm yarn should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
        assert!(overrides.runner.is_none());
    }

    #[test]
    fn env_pm_value_parses_when_cli_absent() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("bun"),
            },
            ..OverrideSources::default()
        })
        .expect("RUNNER_PM=bun should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Bun);
        assert_eq!(pm.origin, OverrideOrigin::EnvVar);
    }

    #[test]
    fn cli_wins_over_env() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: Some("bun"),
            },
            ..OverrideSources::default()
        })
        .expect("both sources should parse");

        let pm = overrides.pm.expect("pm override should be present");
        assert_eq!(pm.pm, PackageManager::Yarn);
        assert_eq!(pm.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn empty_env_is_treated_as_unset() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some(""),
            },
            ..OverrideSources::default()
        })
        .expect("empty env should parse as no override");

        assert!(overrides.pm.is_none());
    }

    #[test]
    fn cli_runner_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            runner: SourceValue {
                cli: Some("just"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("--runner just should parse");

        let runner: RunnerOverride = overrides.runner.expect("runner override should be present");
        assert_eq!(runner.runner, TaskRunner::Just);
        assert_eq!(runner.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn unknown_pm_label_errors_with_valid_value_list() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("zoot"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown PM should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown package manager"));
        assert!(msg.contains("npm"));
        assert!(msg.contains("pnpm"));
    }

    #[test]
    fn unknown_runner_label_errors_with_valid_value_list() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            runner: SourceValue {
                cli: Some("zoot"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown runner should error");

        let msg = format!("{err}");
        assert!(msg.contains("unknown task runner"));
        assert!(msg.contains("turbo"));
    }

    #[test]
    fn unknown_pm_env_value_names_env_source() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("zoot"),
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown PM via env should error");

        let msg = format!("{err}");
        assert!(
            msg.contains("RUNNER_PM"),
            "should name the env source: {msg}"
        );
        assert!(msg.contains("unknown package manager"));
    }

    #[test]
    fn unknown_pm_cli_value_names_cli_source() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("zoot"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown PM via CLI should error");

        let msg = format!("{err}");
        assert!(msg.contains("--pm"), "should name the CLI source: {msg}");
    }

    #[test]
    fn multiline_env_pm_value_is_sanitized_and_hinted() {
        // The PowerShell unquoted-assignment footgun: `$env:RUNNER_PM=deno`
        // executes deno and captures its REPL banner (ANSI codes included)
        // into the variable.
        let banner = "Deno 2.8.2 exit using ctrl+d\n\u{1b}[33mREPL is running\u{1b}[0m";
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some(banner),
            },
            ..OverrideSources::default()
        })
        .expect_err("captured banner should error");

        let msg = format!("{err}");
        assert!(!msg.contains('\u{1b}'), "raw ESC byte must not leak: {msg}");
        assert!(
            msg.contains("captured command output"),
            "should hint at the footgun: {msg}"
        );
        assert!(
            msg.contains("$env:RUNNER_PM='pnpm'"),
            "should show the quoted PowerShell spelling: {msg}"
        );
    }

    #[test]
    fn oversized_pm_value_is_truncated() {
        let huge = "z".repeat(500);
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some(&huge),
            },
            ..OverrideSources::default()
        })
        .expect_err("oversized garbage should error");

        let msg = format!("{err}");
        assert!(msg.contains('…'), "long values should be truncated: {msg}");
        assert!(
            !msg.contains(&"z".repeat(100)),
            "the full 500-char value must not be rendered: {msg}"
        );
    }

    #[test]
    fn unknown_runner_env_value_names_env_source() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            runner: SourceValue {
                cli: None,
                env: Some("zoot"),
            },
            ..OverrideSources::default()
        })
        .expect_err("unknown runner via env should error");

        let msg = format!("{err}");
        assert!(
            msg.contains("RUNNER_RUNNER"),
            "should name the env source: {msg}"
        );
        assert!(msg.contains("unknown task runner"));
    }

    #[test]
    fn lenient_env_pm_garbage_degrades_to_warning() {
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("Deno 2.8.2 exit using ctrl+d\n\u{1b}[33mbanner"),
            },
            ..OverrideSources::default()
        })
        .expect("lenient pass must absorb env garbage");

        assert!(overrides.pm.is_none(), "garbage override must be blanked");
        assert_eq!(warnings.len(), 1);
        match &warnings[0] {
            DetectionWarning::InvalidEnvOverride { var, raw, .. } => {
                assert_eq!(*var, "RUNNER_PM");
                assert!(!raw.contains('\u{1b}'), "raw must be sanitized: {raw}");
            }
            other => panic!("expected InvalidEnvOverride, got {other:?}"),
        }
        let detail = warnings[0].detail();
        assert!(detail.contains("ignored"), "detail: {detail}");
    }

    #[test]
    fn lenient_env_bool_typo_warns_and_is_ignored() {
        // `RUNNER_KEEP_GOING=flase` (typo'd "false") used to read as
        // truthy, the opposite of the user's intent. It must warn and
        // stay unset instead.
        use crate::chain::FailurePolicy;
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            keep_going: ExplainSource {
                cli: false,
                env: Some("flase"),
            },
            quiet: QuietSource {
                cli: 0,
                env: Some("disabled"),
            },
            ..OverrideSources::default()
        })
        .expect("lenient pass must absorb boolean env garbage");

        assert_eq!(overrides.failure_policy, FailurePolicy::FailFast);
        assert!(
            overrides.shows_progress(),
            "typo'd RUNNER_QUIET must not enable quiet"
        );
        let vars: Vec<&str> = warnings
            .iter()
            .map(|w| match w {
                DetectionWarning::InvalidEnvOverride { var, .. } => *var,
                other => panic!("expected InvalidEnvOverride, got {other:?}"),
            })
            .collect();
        assert_eq!(vars, ["RUNNER_QUIET", "RUNNER_KEEP_GOING"]);
    }

    #[test]
    fn lenient_env_bool_recognized_tokens_pass_clean() {
        use crate::chain::FailurePolicy;
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            keep_going: ExplainSource {
                cli: false,
                env: Some("YES"),
            },
            explain: ExplainSource {
                cli: false,
                env: Some("off"),
            },
            ..OverrideSources::default()
        })
        .expect("recognized boolean tokens should parse");

        assert!(warnings.is_empty());
        assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
        assert!(!overrides.explain);
    }

    #[test]
    fn lenient_cli_garbage_still_errors() {
        ResolutionOverrides::from_sources_lenient(OverrideSources {
            pm: SourceValue {
                cli: Some("zoot"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("explicit CLI garbage must stay fatal even leniently");
    }

    #[test]
    fn lenient_valid_env_produces_no_warnings() {
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("bun"),
            },
            ..OverrideSources::default()
        })
        .expect("valid env value should parse");

        assert!(warnings.is_empty());
        assert_eq!(
            overrides.pm.expect("pm should be set").pm,
            PackageManager::Bun
        );
    }

    #[test]
    fn lenient_cli_value_shadows_env_garbage() {
        // Strict precedence never parses a CLI-shadowed env value, so the
        // lenient pass must not warn about it either.
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: Some("complete garbage\nwith newlines"),
            },
            ..OverrideSources::default()
        })
        .expect("CLI value should shadow env garbage");

        assert!(
            warnings.is_empty(),
            "shadowed env must not warn: {warnings:?}"
        );
        assert_eq!(
            overrides.pm.expect("pm should be set").pm,
            PackageManager::Yarn
        );
    }

    #[test]
    fn lenient_covers_runner_fallback_and_mismatch_vars() {
        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(OverrideSources {
            runner: SourceValue {
                cli: None,
                env: Some("bogus-runner"),
            },
            fallback: SourceValue {
                cli: None,
                env: Some("bogus-policy"),
            },
            on_mismatch: SourceValue {
                cli: None,
                env: Some("bogus-mismatch"),
            },
            ..OverrideSources::default()
        })
        .expect("lenient pass must absorb all env-sourced garbage");

        assert!(overrides.runner.is_none());
        let vars: Vec<&str> = warnings
            .iter()
            .map(|w| match w {
                DetectionWarning::InvalidEnvOverride { var, .. } => *var,
                other => panic!("expected InvalidEnvOverride, got {other:?}"),
            })
            .collect();
        assert_eq!(
            vars,
            vec!["RUNNER_RUNNER", "RUNNER_FALLBACK", "RUNNER_ON_MISMATCH"]
        );
    }

    #[test]
    fn pm_label_that_names_a_runner_suggests_runner_flag() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("mise"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("`--pm mise` should error; mise is a task runner");

        let msg = format!("{err}");
        assert!(
            msg.contains("task runner"),
            "error should call out the category mismatch: {msg}"
        );
        assert!(
            msg.contains("--runner mise"),
            "error should suggest the correct flag: {msg}"
        );
    }

    #[test]
    fn runner_label_that_names_a_pm_suggests_pm_flag() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            runner: SourceValue {
                cli: Some("pnpm"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("`--runner pnpm` should error; pnpm is a package manager");

        let msg = format!("{err}");
        assert!(
            msg.contains("package manager"),
            "error should call out the category mismatch: {msg}"
        );
        assert!(
            msg.contains("--pm pnpm"),
            "error should suggest the correct flag: {msg}"
        );
    }

    #[test]
    fn bundler_alias_bundle_is_accepted() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("bundle"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("`bundle` should alias to bundler");

        assert_eq!(
            overrides.pm.expect("pm should be present").pm,
            PackageManager::Bundler,
        );
    }

    #[test]
    fn go_task_alias_is_accepted() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            runner: SourceValue {
                cli: Some("go-task"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("`go-task` should alias to GoTask");

        assert_eq!(
            overrides.runner.expect("runner should be present").runner,
            TaskRunner::GoTask,
        );
    }

    fn loaded_config_with_node(node: &str) -> LoadedConfig {
        LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
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
    fn config_loaded_value_populates_pm_by_ecosystem() {
        let loaded = loaded_config_with_node("bun");
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
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
            warnings: Vec::new(),
            config: RunnerConfig {
                pm: PmSection {
                    node: None,
                    python: Some("uv".to_owned()),
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
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
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("cargo is not a node-script PM");
        assert!(format!("{err}").contains("cannot dispatch package.json scripts"));
    }

    fn loaded_from_toml(dir: &TempDir, body: &str) -> LoadedConfig {
        std::fs::write(dir.path().join(crate::config::CONFIG_FILENAME), body).expect("seed config");
        crate::config::load(dir.path())
            .expect("config should parse")
            .expect("config should be present")
    }

    #[test]
    fn a_task_runner_section_feeds_no_runner_policy() {
        let dir = TempDir::new("resolver-task-runner-section");
        let loaded = loaded_from_toml(&dir, "[task_runner]\nprefer = [\"just\", \"zoot\"]\n");
        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                DetectionWarning::UnknownConfigKey { path } if path == "task_runner"
            )),
            "{:?}",
            loaded.warnings
        );

        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("an unknown section is never validated");

        assert!(overrides.prefer_sources.is_empty());
        assert!(overrides.prefer_runners.is_empty());
    }

    fn config_with_tasks(tasks: crate::config::TasksSection) -> LoadedConfig {
        LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                tasks,
                ..RunnerConfig::default()
            },
        }
    }

    #[test]
    fn tasks_prefer_parses_pm_and_runner_labels() {
        use crate::config::TasksSection;
        use crate::types::TaskSource;

        let loaded = config_with_tasks(TasksSection {
            prefer: vec!["bun".to_string(), "turbo".to_string()],
            ..TasksSection::default()
        });
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("tasks.prefer of known labels should parse");

        // `bun` (a package manager) maps to its package.json source; `turbo`
        // (a runner) to turbo.json, proving the unified label vocabulary.
        assert_eq!(
            overrides.prefer_sources,
            vec![TaskSource::PackageJson, TaskSource::TurboJson],
        );
        // The deprecated list is left empty when `[tasks]` drives selection.
        assert!(overrides.prefer_runners.is_empty());
    }

    #[test]
    fn tasks_prefer_expands_deno_to_both_its_sources() {
        use crate::config::TasksSection;
        use crate::types::TaskSource;

        let loaded = config_with_tasks(TasksSection {
            prefer: vec!["deno".to_string()],
            ..TasksSection::default()
        });
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("deno should parse");

        assert_eq!(
            overrides.prefer_sources,
            vec![TaskSource::DenoJson, TaskSource::PackageJson],
        );
    }

    #[test]
    fn tasks_prefer_rejects_unknown_label() {
        use crate::config::TasksSection;

        let loaded = config_with_tasks(TasksSection {
            prefer: vec!["zoot".to_string()],
            ..TasksSection::default()
        });
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("unknown source label must error at parse time");

        let msg = format!("{err}");
        assert!(msg.contains("[tasks].prefer"), "got: {msg}");
        assert!(msg.contains("unknown source"), "got: {msg}");
        assert!(msg.contains("zoot"), "got: {msg}");
    }

    #[test]
    fn tasks_prefer_applies_beside_a_task_runner_section() {
        use crate::types::TaskSource;

        let dir = TempDir::new("resolver-tasks-prefer-beside-task-runner");
        let loaded = loaded_from_toml(
            &dir,
            "[task_runner]\nprefer = [\"just\"]\n\n[tasks]\nprefer = [\"turbo\"]\n",
        );
        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                DetectionWarning::UnknownConfigKey { path } if path == "task_runner"
            )),
            "{:?}",
            loaded.warnings
        );

        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("config should parse");

        assert_eq!(overrides.prefer_sources, vec![TaskSource::TurboJson]);
        assert!(overrides.prefer_runners.is_empty());
    }

    #[test]
    fn tasks_prefer_of_a_sourceless_label_parses_to_no_ranking() {
        use crate::config::TasksSection;

        let loaded = config_with_tasks(TasksSection {
            prefer: vec!["nx".to_string()],
            ..TasksSection::default()
        });
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("a known label with no task source parses");

        assert!(overrides.prefer_sources.is_empty());
    }

    #[test]
    fn tasks_overrides_parse_into_per_task_pins() {
        use std::collections::BTreeMap;

        use crate::config::TasksSection;
        use crate::types::TaskSource;

        let loaded = config_with_tasks(TasksSection {
            prefer: Vec::new(),
            overrides: BTreeMap::from([
                ("build".to_string(), "turbo".to_string()),
                ("dev".to_string(), "bun".to_string()),
            ]),
            ..TasksSection::default()
        });
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("overrides should parse");

        assert_eq!(
            overrides.task_source_overrides.get("build"),
            Some(&vec![TaskSource::TurboJson]),
        );
        assert_eq!(
            overrides.task_source_overrides.get("dev"),
            Some(&vec![TaskSource::PackageJson]),
        );
    }

    #[test]
    fn tasks_overrides_reject_a_label_with_no_task_source() {
        use std::collections::BTreeMap;

        use crate::config::TasksSection;

        // `nx` is a known runner but has no extractable task source, so it
        // can't be pinned to.
        let loaded = config_with_tasks(TasksSection {
            prefer: Vec::new(),
            overrides: BTreeMap::from([("build".to_string(), "nx".to_string())]),
            ..TasksSection::default()
        });
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("a pin to a sourceless runner must error");

        assert!(format!("{err}").contains("no task source"), "got: {err}");
    }

    #[test]
    fn on_mismatch_label_parses_three_values() {
        use super::MismatchPolicy;
        use super::policies::parse_mismatch_label;

        assert_eq!(parse_mismatch_label("warn").unwrap(), MismatchPolicy::Warn);
        assert_eq!(
            parse_mismatch_label("error").unwrap(),
            MismatchPolicy::Error
        );
        assert_eq!(
            parse_mismatch_label("ignore").unwrap(),
            MismatchPolicy::Ignore
        );
        assert!(parse_mismatch_label("nope").is_err());
    }

    #[test]
    fn from_sources_builder_is_ergonomic_for_partial_overrides() {
        // Demonstrates the canonical idiom: construct only the fields
        // that matter, default the rest. All sibling tests in this module
        // use the same shape.
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some("yarn"),
                env: None,
            },
            explain: ExplainSource {
                cli: true,
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("structured override should parse");

        assert_eq!(
            overrides.pm.expect("pm override should be present").pm,
            PackageManager::Yarn
        );
        assert!(overrides.explain);
        assert!(overrides.runner.is_none());
    }

    #[test]
    fn quiet_from_env_is_truthy() {
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            quiet: QuietSource {
                cli: 0,
                env: Some("1"),
            },
            ..OverrideSources::default()
        })
        .expect("structured override should parse");

        assert!(!overrides.shows_progress());
    }

    #[test]
    fn cli_quiet_count_wins_over_env_level() {
        // Regression (F4): CLI > env — a passed `-q` (count 1) must NOT be
        // escalated to Silent by `RUNNER_QUIET=3`. `silences_warnings` is true
        // only at VeryQuiet+, so it discriminates level 1 from level 3.
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            quiet: QuietSource {
                cli: 1,
                env: Some("3"),
            },
            ..OverrideSources::default()
        })
        .expect("should parse");
        assert!(!overrides.shows_progress(), "-q still silences runner");
        assert!(
            !overrides.silences_warnings(),
            "an explicit -q (level 1) must not be escalated to Silent by RUNNER_QUIET=3",
        );

        // With no CLI flag, the env level applies in full.
        let env_only = ResolutionOverrides::from_sources(&OverrideSources {
            quiet: QuietSource {
                cli: 0,
                env: Some("3"),
            },
            ..OverrideSources::default()
        })
        .expect("should parse");
        assert!(
            env_only.silences_warnings(),
            "RUNNER_QUIET=3 with no -q resolves to Silent",
        );
    }

    #[test]
    fn runner_quiet_env_saturates_large_numbers() {
        // `RUNNER_QUIET=999` exceeds u8 but must clamp to the named maximum,
        // not fall through to invalid.
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            quiet: QuietSource {
                cli: 0,
                env: Some("999"),
            },
            ..OverrideSources::default()
        })
        .expect("should parse");
        assert!(
            overrides.silences_warnings(),
            "RUNNER_QUIET=999 clamps to Mute",
        );
        assert_eq!(overrides.quiet_level, crate::tool::QuietLevel::Mute);
    }

    #[test]
    fn quiet_garbage_env_fails_like_every_other_setting() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            quiet: QuietSource {
                cli: 0,
                env: Some("loud"),
            },
            ..OverrideSources::default()
        })
        .expect_err("the strict pass refuses a bad RUNNER_QUIET");
        assert!(format!("{err}").contains("RUNNER_QUIET=loud"), "{err}");
    }

    #[test]
    fn host_stream_garbage_env_fails_like_every_other_setting() {
        let sources = || OverrideSources {
            host_stream: SourceValue {
                cli: None,
                env: Some("stdrr"),
            },
            ..OverrideSources::default()
        };

        let err = ResolutionOverrides::from_sources(&sources()).expect_err(
            "the strict pass refuses a bad RUNNER_HOST_STREAM as it refuses a bad RUNNER_PM",
        );
        let msg = format!("{err}");
        assert!(msg.contains("RUNNER_HOST_STREAM"), "{msg}");
        assert!(msg.contains("stdrr"), "{msg}");

        let (overrides, warnings) = ResolutionOverrides::from_sources_lenient(sources())
            .expect("the lenient pass absorbs it");
        assert_eq!(overrides.host_stream, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        match &warnings[0] {
            DetectionWarning::InvalidEnvOverride { var, raw, .. } => {
                assert_eq!(*var, "RUNNER_HOST_STREAM");
                assert_eq!(raw, "stdrr");
            }
            other => panic!("expected InvalidEnvOverride, got {other:?}"),
        }
    }

    #[test]
    fn host_stream_bad_cli_flag_still_errors() {
        // The explicit flag stays strict (clap normally validates it; the
        // resolver is the backstop).
        let result = ResolutionOverrides::from_sources(&OverrideSources {
            host_stream: SourceValue {
                cli: Some("bogus"),
                env: None,
            },
            ..OverrideSources::default()
        });
        assert!(result.is_err(), "a bad --host-stream value must error");
    }

    #[test]
    fn explicit_quiet_merges_with_runner_config_quietest_wins() {
        use crate::config::{HostOutputSection, RunnerOutputSection};
        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                runner: RunnerOutputSection {
                    warnings: Some(false),
                    progress: Some(true),
                    ..RunnerOutputSection::default()
                },
                host: HostOutputSection {
                    diagnostics: Some("reduced".to_string()),
                    stream: None,
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            quiet: QuietSource { cli: 1, env: None },
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("resolves");

        assert!(
            !overrides.shows_warnings(),
            "config hides warnings under -q"
        );
        assert!(
            !overrides.shows_progress(),
            "-q hides progress despite config"
        );
        assert!(overrides.shows_errors());
        assert_eq!(
            overrides.output_policy.host_diagnostics,
            crate::tool::HostDiagnostics::Reduced,
            "config host reduction survives -q",
        );
    }

    #[test]
    fn per_task_runner_switches_layer_under_the_global_policy() {
        let mut overrides = ResolutionOverrides::default();
        overrides.task_verbosity.insert(
            "greet".to_string(),
            TaskVerbosity {
                progress: Some(false),
                task_timing: Some(false),
                ..TaskVerbosity::default()
            },
        );
        overrides.task_verbosity.insert(
            "make:greet".to_string(),
            TaskVerbosity {
                task_timing: Some(true),
                ..TaskVerbosity::default()
            },
        );

        assert!(!overrides.shows_progress_for("root:make#greet"));
        assert!(overrides.shows_task_timing_for("root:make#greet"));
        assert!(overrides.emits_groups_for("root:make#greet"));
        assert!(overrides.shows_progress_for("root:make#other"));

        let quiet = ResolutionOverrides {
            output_policy: crate::tool::OutputPolicy::from_quiet(crate::tool::QuietLevel::Quiet),
            task_verbosity: overrides.task_verbosity.clone(),
            ..ResolutionOverrides::default()
        };
        assert!(
            !quiet.shows_task_timing_for("root:make#greet"),
            "a per-task true never re-enables what -q hides",
        );
    }

    #[test]
    fn explicit_inherit_host_stream_outranks_config_and_task_streams() {
        let mut overrides = ResolutionOverrides {
            host_stream: Some(crate::tool::Stream::Inherit),
            host_stream_config: crate::tool::Stream::Stderr,
            ..ResolutionOverrides::default()
        };
        overrides.task_verbosity.insert(
            "make:greet".to_string(),
            TaskVerbosity {
                stream: Some(crate::tool::Stream::Stderr),
                ..TaskVerbosity::default()
            },
        );

        assert_eq!(
            overrides.host_verbosity_for("make:greet").stream,
            crate::tool::Stream::Inherit
        );
    }

    #[test]
    fn qualified_task_streams_deep_merge_over_bare_task() {
        let mut overrides = ResolutionOverrides::default();
        overrides.task_verbosity.insert(
            "greet".to_string(),
            TaskVerbosity {
                stdout: Some(crate::tool::TaskStream::Discard),
                stderr: Some(crate::tool::TaskStream::Discard),
                ..TaskVerbosity::default()
            },
        );
        overrides.task_verbosity.insert(
            "make:greet".to_string(),
            TaskVerbosity {
                stdout: Some(crate::tool::TaskStream::Inherit),
                ..TaskVerbosity::default()
            },
        );

        assert_eq!(
            overrides.task_streams_for("root:make#greet"),
            (
                crate::tool::TaskStream::Inherit,
                crate::tool::TaskStream::Discard
            )
        );
    }

    #[test]
    fn member_task_keys_layer_over_bare_and_source_keys() {
        // A member task dispatches under its FQN (`rfc:package.json#site`);
        // config may address it by bare name, `package.json:site`,
        // `rfc:site`, or that FQN, most specific winning per axis.
        let mut overrides = ResolutionOverrides::default();
        let entry = |stdout, stderr| TaskVerbosity {
            stdout,
            stderr,
            ..TaskVerbosity::default()
        };
        overrides.task_verbosity.insert(
            "site".to_string(),
            entry(
                Some(crate::tool::TaskStream::Discard),
                Some(crate::tool::TaskStream::Discard),
            ),
        );
        overrides.task_verbosity.insert(
            "rfc:site".to_string(),
            entry(Some(crate::tool::TaskStream::Inherit), None),
        );

        assert_eq!(
            overrides.task_streams_for("rfc:package.json#site"),
            (
                crate::tool::TaskStream::Inherit,
                crate::tool::TaskStream::Discard
            )
        );
        assert_eq!(
            overrides.task_streams_for("web:package.json#site"),
            (
                crate::tool::TaskStream::Discard,
                crate::tool::TaskStream::Discard
            ),
            "another member only inherits the bare-name entry",
        );

        overrides.task_verbosity.insert(
            "rfc:package.json#site".to_string(),
            entry(None, Some(crate::tool::TaskStream::Inherit)),
        );
        assert_eq!(
            overrides.task_streams_for("rfc:package.json#site"),
            (
                crate::tool::TaskStream::Inherit,
                crate::tool::TaskStream::Inherit
            )
        );
    }

    #[test]
    fn parse_override_trims_whitespace_in_env_and_cli() {
        // Whitespace in env values is common when shell-export patterns
        // leave trailing newlines or quoted values pad arguments. The
        // override parser must tolerate this so `RUNNER_PM=" pnpm "`
        // works the same as `RUNNER_PM=pnpm` instead of erroring on an
        // "unknown package manager" with the padded label.
        let from_env = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some(" pnpm "),
            },
            ..OverrideSources::default()
        })
        .expect("padded env value should parse after trimming");
        assert_eq!(
            from_env.pm.expect("pm should be present").pm,
            PackageManager::Pnpm
        );

        let from_cli = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: Some(" yarn\n"),
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("padded CLI value should parse after trimming");
        assert_eq!(
            from_cli.pm.expect("pm should be present").pm,
            PackageManager::Yarn
        );

        // Whitespace-only values are treated as unset (same as empty
        // strings); without this, `RUNNER_PM="   "` would fail with
        // "unknown package manager \"\"" after the trim.
        let blank = ResolutionOverrides::from_sources(&OverrideSources {
            pm: SourceValue {
                cli: None,
                env: Some("   "),
            },
            ..OverrideSources::default()
        })
        .expect("whitespace-only env should parse as no override");
        assert!(blank.pm.is_none());
    }

    #[test]
    fn is_env_truthy_is_case_insensitive_for_falsy_values() {
        use super::policies::is_env_truthy;

        // Falsy values in any case should be falsy.
        assert!(!is_env_truthy("false"));
        assert!(!is_env_truthy("FALSE"));
        assert!(!is_env_truthy("False"));
        assert!(!is_env_truthy("no"));
        assert!(!is_env_truthy("NO"));
        assert!(!is_env_truthy("off"));
        assert!(!is_env_truthy("OFF"));
        assert!(!is_env_truthy("Off"));
        assert!(!is_env_truthy("0"));
        assert!(!is_env_truthy(""));

        // Surrounding whitespace shouldn't flip a falsy value.
        assert!(!is_env_truthy("  false  "));
        assert!(!is_env_truthy("\nfalse\n"));

        // Anything else is truthy.
        assert!(is_env_truthy("1"));
        assert!(is_env_truthy("true"));
        assert!(is_env_truthy("yes"));
        assert!(is_env_truthy("on"));
        assert!(is_env_truthy("anything"));
    }

    #[test]
    fn deno_config_value_fills_the_node_slot_and_resolves_for_node_scripts() {
        let loaded = loaded_config_with_node("deno");
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("deno config should parse");

        let node = overrides
            .pm_by_ecosystem
            .get(&Ecosystem::Node)
            .expect("[pm].node is keyed by the key's ecosystem");
        assert_eq!(node.pm, PackageManager::Deno);
        assert!(!overrides.pm_by_ecosystem.contains_key(&Ecosystem::Deno));
    }

    fn test_loaded_config_with_chain(
        keep_going: Option<bool>,
        kill_on_fail: Option<bool>,
    ) -> LoadedConfig {
        use crate::config::ChainSection;
        LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                chain: ChainSection {
                    keep_going,
                    kill_on_fail,
                },
                ..RunnerConfig::default()
            },
        }
    }

    #[test]
    fn from_sources_resolves_cli_keep_going() {
        use crate::chain::FailurePolicy;
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: true,
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect("resolves");
        assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
    }

    #[test]
    fn from_sources_env_overrides_config_for_failure_policy() {
        use crate::chain::FailurePolicy;
        let loaded = test_loaded_config_with_chain(Some(false), None);
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: false,
                env: Some("1"),
            },
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("resolves");
        assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
    }

    #[test]
    fn from_sources_rejects_both_keep_going_and_kill_on_fail() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: true,
                env: None,
            },
            kill_on_fail: ExplainSource {
                cli: true,
                env: None,
            },
            ..OverrideSources::default()
        })
        .expect_err("conflict must error");
        let downcast = err.downcast_ref::<ResolveError>();
        assert!(
            matches!(
                downcast,
                Some(ResolveError::ConflictingFailurePolicy { .. })
            ),
            "expected ConflictingFailurePolicy, got: {err:#}",
        );
    }

    #[test]
    fn from_sources_cli_flag_beats_opposite_config_polarity() {
        // `-k` must override `[chain] kill_on_fail = true`, not collide
        // with it: the config polarity has no CLI negation flag, so a
        // cross-source conflict error would make it uncancellable from
        // the command line.
        use crate::chain::FailurePolicy;
        let loaded = test_loaded_config_with_chain(None, Some(true));
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: true,
                env: None,
            },
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("CLI -k must win over config kill_on_fail");
        assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
    }

    #[test]
    fn from_sources_env_truthy_beats_opposite_config_polarity() {
        use crate::chain::FailurePolicy;
        let loaded = test_loaded_config_with_chain(Some(true), None);
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            kill_on_fail: ExplainSource {
                cli: false,
                env: Some("1"),
            },
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("env kill_on_fail must win over config keep_going");
        assert_eq!(overrides.failure_policy, FailurePolicy::KillOnFail);
    }

    #[test]
    fn from_sources_cli_flag_beats_opposite_env_polarity() {
        use crate::chain::FailurePolicy;
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: true,
                env: None,
            },
            kill_on_fail: ExplainSource {
                cli: false,
                env: Some("1"),
            },
            ..OverrideSources::default()
        })
        .expect("CLI -k must win over RUNNER_KILL_ON_FAIL=1");
        assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
    }

    #[test]
    fn from_sources_env_false_overrides_config_true_for_failure_policy() {
        use crate::chain::FailurePolicy;
        let loaded = test_loaded_config_with_chain(Some(true), None);
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: false,
                env: Some("0"),
            },
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("resolves");
        assert_eq!(overrides.failure_policy, FailurePolicy::FailFast);
    }

    #[test]
    fn from_sources_env_false_neutralises_config_conflict() {
        use crate::chain::FailurePolicy;
        let loaded = test_loaded_config_with_chain(Some(true), Some(true));
        let overrides = ResolutionOverrides::from_sources(&OverrideSources {
            kill_on_fail: ExplainSource {
                cli: false,
                env: Some("false"),
            },
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("env=false on one side should neutralise the [chain] config conflict");
        assert_eq!(overrides.failure_policy, FailurePolicy::KeepGoing);
    }

    #[test]
    fn from_sources_rejects_both_env_vars_truthy() {
        let err = ResolutionOverrides::from_sources(&OverrideSources {
            keep_going: ExplainSource {
                cli: false,
                env: Some("1"),
            },
            kill_on_fail: ExplainSource {
                cli: false,
                env: Some("1"),
            },
            ..OverrideSources::default()
        })
        .expect_err("env-layer conflict must error");
        let downcast = err.downcast_ref::<ResolveError>();
        assert!(
            matches!(
                downcast,
                Some(ResolveError::ConflictingFailurePolicy { source: "env vars" })
            ),
            "expected env-layer ConflictingFailurePolicy, got: {err:#}",
        );
    }
}
