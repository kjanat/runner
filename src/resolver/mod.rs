//! Resolution of package managers and task sources for `runner run`.
//!
//! The resolver consumes a [`ProjectContext`] (signals discovered during
//! detection) plus a [`ResolutionOverrides`] bundle (CLI flags, env vars,
//! and, in later phases, a `runner.toml`) and returns a single decision
//! tagged with the chain step that produced it.
//!
//! Chain order (lower wins):
//!
//! 1. Qualified syntax (`turbo.json:build`), handled in `cmd::run` today.
//! 2. CLI flag (`--pm`, `--runner`).
//! 3. Environment variable (`RUNNER_PM`, `RUNNER_RUNNER`).
//! 4. Project config (`./runner.toml`), Phase 3.
//! 5. Manifest declaration (`packageManager`, `devEngines.packageManager`), Phase 4.
//! 6. Lockfile (current behavior; lives in [`crate::detect`]).
//! 7. `PATH` probe in canonical order, Phase 5.
//! 8. Terminal, error with actionable guidance, Phase 5.

//! # Module layout
//!
//! - [`types`], data shapes ([`Resolver`], [`ResolutionOverrides`], the
//!   policy enums, override-builder helpers).
//! - [`resolve`], the resolution algorithm itself: `impl Resolver` plus
//!   the manifest / lockfile cross-checks.
//! - [`overrides`], `impl ResolutionOverrides` and the CLI/env parsers
//!   that feed it.
//! - [`policies`], pure string→enum parsing for the `FallbackPolicy`,
//!   `MismatchPolicy`, and `FailurePolicy` knobs.
//! - [`error`], the `ResolveError` type surfaced to callers.
//! - [`probe`], `$PATH` probing for the fallback step.

mod error;
mod overrides;
mod policies;
mod probe;
mod resolve;
mod types;

pub(crate) use error::{DevEnginesFailReason, ResolveError};
/// Re-export of the standalone `runner.toml` validator backing
/// `cmd::config::validate`; see [`overrides::validate_config`].
pub(crate) use overrides::validate_config;
/// Re-export of the canonical Node PATH-probe order so the doctor's
/// schema layer doesn't carry its own copy.
pub(crate) use probe::NODE_PROBE_ORDER;
/// Re-export of the pure-function probe variant for the `doctor` subcommand.
/// Lets `cmd::doctor` exercise the same PATH walk the resolver uses without
/// owning the env-reading logic.
pub(crate) use probe::probe_in as probe_path_for_doctor;
/// Re-exported for unit tests that need to construct override state
/// directly (e.g. `cmd::install::tests`); production code receives
/// overrides fully built by [`ResolutionOverrides::from_cli_and_env`].
#[cfg(test)]
pub(crate) use types::PmOverride;
pub(crate) use types::{
    CollisionPolicy, DiagnosticFlags, FallbackPolicy, MismatchPolicy, OverrideOrigin,
    ResolutionOverrides, ResolutionStep, ResolvedPm, Resolver, ScriptPolicy,
};

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
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::types::{
        ExplainSource, OverrideSources, PmOverride, QuietSource, ResolutionStep, RunnerOverride,
        SourceValue,
    };
    use super::{FallbackPolicy, OverrideOrigin, ResolutionOverrides, ResolveError, Resolver};
    use crate::config::{LoadedConfig, PmSection, RunnerConfig};
    use crate::tool::test_support::TempDir;
    use crate::types::{DetectionWarning, Ecosystem, PackageManager, ProjectContext, TaskRunner};

    fn context(package_managers: Vec<PackageManager>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers,
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Like [`context`], but rooted in a fresh temp dir instead of `"."`.
    ///
    /// Tests that exercise the manifest-blind steps, lockfile and the
    /// fallback policies, must not anchor at the repo checkout: the
    /// manifest walk (`detect_pm_from_manifest` / `find_manifest_upwards`)
    /// starts at `ctx.root`, and runner's own `package.json` declares
    /// `"packageManager": "bun@…"`, which outranks the synthetic context
    /// and flips the step under test. The [`TempDir`] rides along so the
    /// directory outlives the context.
    fn isolated_context(package_managers: Vec<PackageManager>) -> (TempDir, ProjectContext) {
        let dir = TempDir::new("resolver-isolated");
        let mut ctx = context(package_managers);
        ctx.root = dir.path().to_path_buf();
        (dir, ctx)
    }

    fn resolver<'ctx>(
        ctx: &'ctx ProjectContext,
        overrides: &'ctx ResolutionOverrides,
    ) -> Resolver<'ctx> {
        Resolver::new(ctx, overrides)
    }

    fn with_pm_override(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..ResolutionOverrides::default()
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
            pm_by_ecosystem: map,
            ..ResolutionOverrides::default()
        }
    }

    #[test]
    fn resolves_detected_node_pm_via_lockfile() {
        let (_dir, ctx) = isolated_context(vec![PackageManager::Pnpm]);
        let overrides = ResolutionOverrides::default();
        let decision = resolver(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_legacy_npm_when_fallback_policy_is_npm() {
        let (_dir, ctx) = isolated_context(vec![]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Npm,
            ..ResolutionOverrides::default()
        };
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("legacy npm fallback should succeed");

        assert_eq!(decision.pm, PackageManager::Npm);
        assert_eq!(decision.via, ResolutionStep::LegacyNpmFallback);
    }

    #[test]
    fn fallback_error_policy_returns_helpful_error_when_no_signal() {
        let (_dir, ctx) = isolated_context(vec![]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Error,
            ..ResolutionOverrides::default()
        };
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail when nothing matches");

        let msg = format!("{err}");
        assert!(msg.contains("no node package manager detected"));
        assert!(msg.contains("--pm"));
    }

    #[test]
    fn fallback_error_policy_is_a_hard_no_signals_found() {
        // `--fallback=error` is the user opting into strict mode. The
        // error must propagate through `cmd::run::run` instead of
        // collapsing into the soft fall-through, so it carries
        // `soft: false`.
        let (_dir, ctx) = isolated_context(vec![]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Error,
            ..ResolutionOverrides::default()
        };
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail");

        assert!(
            matches!(err, ResolveError::NoSignalsFound { soft: false, .. }),
            "error-policy failure must be hard, got: {err:?}"
        );
    }

    #[test]
    fn fallback_probe_with_empty_path_yields_soft_no_signals_found() {
        // The soft case: nothing declared, nothing on PATH. The
        // arbitrary-command fallback in `cmd::run::run` legitimately
        // wants to drop this and try a direct PATH spawn, so the error
        // carries `soft: true`.
        //
        // A TempDir with no `package.json` short-circuits the probe
        // entirely (issue #23 guard), making the assertion
        // deterministic regardless of what's on the host `$PATH`.

        let dir = TempDir::new("resolver-soft-no-signals");
        let mut ctx = context(vec![]);
        ctx.root = dir.path().to_path_buf();

        let overrides = ResolutionOverrides::default();
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("probe with no Node evidence must error");

        assert!(
            matches!(err, ResolveError::NoSignalsFound { soft: true, .. }),
            "probe-policy miss must be the soft variant, got: {err:?}"
        );
    }

    #[test]
    fn fallback_probe_skipped_when_no_package_json_present() {
        // Issue #23: in a non-Node project (e.g., a Go repo with
        // `go.mod` and `.mise.toml`), the resolver must not fall
        // through to a Node PM via PATH. The soft `NoSignalsFound`
        // lets `cmd::run` direct-spawn the target instead of
        // routing through `bun`/`pnpm`/`yarn`/`npm`.

        let dir = TempDir::new("resolver-no-pkgjson");
        // Detected ecosystem signals are non-Node, mirrors what
        // `detect` would produce for a Go project.
        let mut ctx = context(vec![PackageManager::Go]);
        ctx.root = dir.path().to_path_buf();

        let overrides = ResolutionOverrides::default();
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("non-Node project must not yield a Node PM from PATH");

        assert!(
            matches!(err, ResolveError::NoSignalsFound { soft: true, .. }),
            "expected soft NoSignalsFound, got: {err:?}"
        );
    }

    #[test]
    fn fallback_probe_fires_when_package_json_exists() {
        // Pins the legitimate PATH-probe path: a greenfield Node
        // project with `package.json` and no lockfile must still
        // get a Node PM picked from PATH (Bun-test fallback in
        // `cmd::run` depends on this resolving to `Bun`). When no
        // Node PM is on the host PATH we accept the soft error;
        // what we're guarding against is the issue-#23 early
        // return firing despite Node evidence.
        use std::fs;

        let dir = TempDir::new("resolver-greenfield-node");
        fs::write(dir.path().join("package.json"), "{}").expect("package.json should be written");

        let mut ctx = context(vec![]);
        ctx.root = dir.path().to_path_buf();

        let overrides = ResolutionOverrides::default();
        match Resolver::new(&ctx, &overrides).resolve_node_pm() {
            Ok(decision) => assert!(
                matches!(decision.via, ResolutionStep::PathProbe { .. }),
                "expected PathProbe step, got {:?}",
                decision.via,
            ),
            Err(ResolveError::NoSignalsFound { soft: true, .. }) => {
                // Host has no Node PM on PATH; probe ran but
                // returned nothing. The guard didn't trip early.
            }
            Err(e) => panic!("unexpected resolver error: {e:?}"),
        }
    }

    #[test]
    fn prefers_node_pm_over_non_node_primary() {
        let (_dir, ctx) = isolated_context(vec![PackageManager::Cargo, PackageManager::Bun]);
        let overrides = ResolutionOverrides::default();
        let decision = resolver(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn falls_back_to_primary_pm_when_no_node_pm_detected() {
        let (_dir, ctx) = isolated_context(vec![PackageManager::Deno]);
        let overrides = ResolutionOverrides::default();
        let decision = resolver(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Deno);
        assert_eq!(decision.via, ResolutionStep::Lockfile);
    }

    #[test]
    fn cli_override_beats_detected_pm() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Yarn, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

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
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

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
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn cross_ecosystem_pm_override_for_node_scripts_is_a_hard_error() {
        // An explicit `--pm cargo` on a Node project is a
        // misconfiguration to surface, not silently drop: it must return
        // `InvalidOverride` so `main` exits 2 with a clear message.
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_pm_override(PackageManager::Cargo, OverrideOrigin::CliFlag);
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("cargo cannot dispatch package.json scripts");

        assert!(
            matches!(err, ResolveError::InvalidOverride { ref value, .. } if value == "cargo"),
            "expected InvalidOverride for cargo, got: {err:?}",
        );
    }

    #[test]
    fn cli_pm_value_parses_to_overrides() {
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
            !overrides.silences_runner(),
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
    fn config_pm_node_field_overrides_detection() {
        let ctx = context(vec![PackageManager::Pnpm]);
        let overrides = with_config_pm(PackageManager::Yarn, Ecosystem::Node);

        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

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

        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        match decision.via {
            ResolutionStep::Override(OverrideOrigin::CliFlag) => {}
            other => panic!("expected CliFlag origin, got {other:?}"),
        }
    }

    #[test]
    fn config_loaded_value_populates_pm_by_ecosystem() {
        let loaded = loaded_config_with_node("bun");
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("cargo is not a node-script PM");
        assert!(format!("{err}").contains("cannot dispatch package.json scripts"));
    }

    #[test]
    fn manifest_package_manager_field_beats_lockfile_signal() {
        use std::fs;

        use crate::detect::detect;

        let dir = TempDir::new("resolver-manifest-wins");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4.3.0" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("lockfile should be written");

        let ctx = detect(dir.path());
        // Detection picks the lockfile signal as primary; the resolver
        // should override that to honor the manifest declaration.
        assert!(ctx.package_managers.contains(&PackageManager::Pnpm));

        let decision = Resolver::new(&ctx, &ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(decision.via, ResolutionStep::ManifestPackageManager);
        assert_eq!(decision.warnings.len(), 1);
        assert_eq!(decision.warnings[0].source(), "package.json");
        let detail = decision.warnings[0].detail();
        assert!(
            detail.contains("declaration wins"),
            "warning should mention declaration wins: {detail}",
        );
    }

    #[test]
    fn dev_engines_used_when_package_manager_absent() {
        use std::fs;

        use crate::detect::detect;

        let dir = TempDir::new("resolver-dev-engines-only");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "bun", "onFail": "warn" } } }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());
        let decision = Resolver::new(&ctx, &ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        match decision.via {
            ResolutionStep::ManifestDevEngines { .. } => {}
            other => panic!("expected ManifestDevEngines, got {other:?}"),
        }
    }

    #[test]
    fn cli_override_still_beats_manifest_declaration() {
        use std::fs;

        use crate::detect::detect;

        let dir = TempDir::new("resolver-cli-beats-manifest");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4" }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());
        let overrides = with_pm_override(PackageManager::Bun, OverrideOrigin::CliFlag);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(
            decision.via,
            ResolutionStep::Override(OverrideOrigin::CliFlag)
        );
    }

    #[test]
    fn matching_lockfile_and_manifest_produce_no_warning() {
        use std::fs;

        use crate::detect::detect;

        let dir = TempDir::new("resolver-matching-signals");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "pnpm@9" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("lockfile should be written");

        let ctx = detect(dir.path());
        let decision = Resolver::new(&ctx, &ResolutionOverrides::default())
            .resolve_node_pm()
            .expect("resolution should succeed");

        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert_eq!(decision.via, ResolutionStep::ManifestPackageManager);
        assert!(decision.warnings.is_empty());
    }

    fn mismatch_dir(name: &str) -> TempDir {
        use std::fs;

        let dir = TempDir::new(name);
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("pnpm-lock.yaml should be written");
        dir
    }

    #[test]
    fn on_mismatch_warn_emits_warning_and_keeps_declaration() {
        use super::MismatchPolicy;
        use crate::detect::detect;

        let dir = mismatch_dir("mismatch-warn");
        let ctx = detect(dir.path());
        let overrides = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Warn,
            ..ResolutionOverrides::default()
        };
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("warn policy should not bail");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(decision.warnings.len(), 1);
        assert!(decision.warnings[0].detail().contains("declaration wins"));
    }

    #[test]
    fn on_mismatch_ignore_silently_keeps_declaration() {
        use super::MismatchPolicy;
        use crate::detect::detect;

        let dir = mismatch_dir("mismatch-ignore");
        let ctx = detect(dir.path());
        let overrides = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Ignore,
            ..ResolutionOverrides::default()
        };
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("ignore policy should not bail");

        assert_eq!(decision.pm, PackageManager::Yarn);
        assert!(decision.warnings.is_empty());
    }

    #[test]
    fn on_mismatch_error_bails_with_resolve_error() {
        use super::MismatchPolicy;
        use crate::detect::detect;

        let dir = mismatch_dir("mismatch-error");
        let ctx = detect(dir.path());
        let overrides = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Error,
            ..ResolutionOverrides::default()
        };
        let err = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect_err("error policy should bail on mismatch");

        assert!(
            matches!(err, ResolveError::MismatchPolicyError { .. }),
            "expected MismatchPolicyError, got: {err:?}"
        );
    }

    #[test]
    fn prefer_runners_parses_known_labels() {
        use crate::config::{LoadedConfig, RunnerConfig, TaskRunnerSection};

        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                task_runner: TaskRunnerSection {
                    prefer: vec!["just".to_string(), "turbo".to_string()],
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("prefer list of known runners should parse");

        assert_eq!(
            overrides.prefer_runners,
            vec![TaskRunner::Just, TaskRunner::Turbo],
        );
    }

    #[test]
    fn prefer_runners_rejects_unknown_label() {
        use crate::config::{LoadedConfig, RunnerConfig, TaskRunnerSection};

        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                task_runner: TaskRunnerSection {
                    prefer: vec!["zoot".to_string()],
                },
                ..RunnerConfig::default()
            },
        };
        let err = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect_err("unknown runner label must error at parse time");

        let msg = format!("{err}");
        assert!(msg.contains("unknown runner"), "got: {msg}");
        assert!(msg.contains("zoot"), "got: {msg}");
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
    fn tasks_section_supersedes_deprecated_task_runner_prefer() {
        use crate::config::{TaskRunnerSection, TasksSection};
        use crate::types::TaskSource;

        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                task_runner: TaskRunnerSection {
                    prefer: vec!["just".to_string()],
                },
                tasks: TasksSection {
                    prefer: vec!["turbo".to_string()],
                    ..TasksSection::default()
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("config should parse");

        // `[tasks]` wins; the legacy restrictive list is dropped entirely.
        assert_eq!(overrides.prefer_sources, vec![TaskSource::TurboJson]);
        assert!(overrides.prefer_runners.is_empty());
    }

    #[test]
    fn tasks_prefer_of_a_sourceless_label_still_supersedes_legacy_prefer() {
        use crate::config::{TaskRunnerSection, TasksSection};

        // `nx` is a recognized runner label that resolves to no `TaskSource`
        // (it has nothing extractable), so `[tasks].prefer` parses to an
        // empty `Vec`. That must not be mistaken for "`[tasks]` unset" and
        // fall back to the deprecated, more restrictive `[task_runner]`.
        let loaded = LoadedConfig {
            path: PathBuf::from("/test/runner.toml"),
            warnings: Vec::new(),
            config: RunnerConfig {
                task_runner: TaskRunnerSection {
                    prefer: vec!["just".to_string()],
                },
                tasks: TasksSection {
                    prefer: vec!["nx".to_string()],
                    ..TasksSection::default()
                },
                ..RunnerConfig::default()
            },
        };
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("config should parse");

        assert!(overrides.prefer_sources.is_empty());
        assert!(overrides.prefer_runners.is_empty());
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
    fn manifest_on_fail_error_bails_when_binary_missing() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        // onFail=Error + the declared PM is missing from PATH should
        // surface as a fatal error so the user knows their pinned
        // toolchain expectation can't be met. Tested with injected
        // checkers since the real PATH is unpredictable in CI.
        let decl = ManifestPmDecl {
            pm: PackageManager::Yarn,
            source: ManifestSource::DevEngines,
            version: None,
            on_fail: OnFail::Error,
        };
        let mut warnings = Vec::new();

        let err = super::resolve::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| false,
            |_, _| VersionCheck::Unverifiable {
                reason: String::new(),
            },
        )
        .expect_err("Error + missing should bail");

        let msg = format!("{err}");
        assert!(msg.contains("yarn"), "error should name the PM: {msg}");
        assert!(
            msg.contains("not found on PATH"),
            "error should explain: {msg}"
        );
        assert!(
            msg.contains("onFail=error"),
            "error should attribute: {msg}"
        );
    }

    #[test]
    fn manifest_on_fail_error_bails_when_version_mismatched() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        // onFail=Error + version range that doesn't match what's
        // installed → fatal. Tests the path that requires both PATH
        // success and a Mismatch result from the version checker.
        let decl = ManifestPmDecl {
            pm: PackageManager::Pnpm,
            source: ManifestSource::DevEngines,
            version: Some(">=9.0.0".to_string()),
            on_fail: OnFail::Error,
        };
        let mut warnings = Vec::new();

        let err = super::resolve::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| true,
            |_, _| VersionCheck::Mismatch {
                declared: ">=9.0.0".to_string(),
                actual: "8.15.0".to_string(),
            },
        )
        .expect_err("Error + version mismatch should bail");

        let msg = format!("{err}");
        assert!(msg.contains("pnpm"));
        assert!(msg.contains(">=9.0.0"));
        assert!(msg.contains("8.15.0"));
        assert!(msg.contains("onFail=error"));
    }

    #[test]
    fn manifest_on_fail_warn_emits_warning_when_binary_missing() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        let decl = ManifestPmDecl {
            pm: PackageManager::Bun,
            source: ManifestSource::DevEngines,
            version: None,
            on_fail: OnFail::Warn,
        };
        let mut warnings = Vec::new();

        super::resolve::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| false,
            |_, _| VersionCheck::Unverifiable {
                reason: String::new(),
            },
        )
        .expect("Warn should not bail");

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].source(), "package.json");
        assert!(warnings[0].detail().contains("bun"));
    }

    #[test]
    fn manifest_on_fail_warn_emits_warning_on_version_mismatch() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        let decl = ManifestPmDecl {
            pm: PackageManager::Yarn,
            source: ManifestSource::DevEngines,
            version: Some("^4.0.0".to_string()),
            on_fail: OnFail::Warn,
        };
        let mut warnings = Vec::new();

        super::resolve::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| true,
            |_, _| VersionCheck::Mismatch {
                declared: "^4.0.0".to_string(),
                actual: "1.22.22".to_string(),
            },
        )
        .expect("Warn should not bail");

        assert_eq!(warnings.len(), 1);
        let detail = warnings[0].detail();
        assert!(detail.contains("yarn"));
        assert!(detail.contains("^4.0.0"));
        assert!(detail.contains("1.22.22"));
    }

    #[test]
    fn manifest_on_fail_ignore_skips_all_checks() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail};

        // OnFail=Ignore short-circuits before the binary/version checks
        // even run; they should never be called. Use a panicking
        // checker to prove the early return holds.
        let decl = ManifestPmDecl {
            pm: PackageManager::Npm,
            source: ManifestSource::DevEngines,
            version: Some(">=20".to_string()),
            on_fail: OnFail::Ignore,
        };
        let mut warnings = Vec::new();

        super::resolve::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| panic!("presence check should not run when onFail=Ignore"),
            |_, _| panic!("version check should not run when onFail=Ignore"),
        )
        .expect("Ignore should always succeed");

        assert!(warnings.is_empty());
    }

    #[test]
    fn manifest_on_fail_unverifiable_version_continues_without_warning() {
        use crate::tool::node::{ManifestPmDecl, ManifestSource, OnFail, VersionCheck};

        // Version checks that can't run (unparseable range, missing
        // --version output) collapse to Unverifiable. That path must
        // continue silently, not warn or bail; otherwise a partially
        // broken environment blocks dispatch unnecessarily.
        let decl = ManifestPmDecl {
            pm: PackageManager::Yarn,
            source: ManifestSource::DevEngines,
            version: Some("not-a-valid-range".to_string()),
            on_fail: OnFail::Error,
        };
        let mut warnings = Vec::new();

        super::resolve::apply_manifest_on_fail(
            &decl,
            &mut warnings,
            |_| true,
            |_, _| VersionCheck::Unverifiable {
                reason: "unparseable range".to_string(),
            },
        )
        .expect("Unverifiable should continue, not bail");

        assert!(warnings.is_empty());
    }

    #[test]
    fn from_sources_builder_is_ergonomic_for_partial_overrides() {
        // Demonstrates the canonical idiom: construct only the fields
        // that matter, default the rest. All sibling tests in this module
        // use the same shape.
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            quiet: QuietSource {
                cli: 0,
                env: Some("1"),
            },
            ..OverrideSources::default()
        })
        .expect("structured override should parse");

        assert!(overrides.silences_runner());
    }

    #[test]
    fn parse_override_trims_whitespace_in_env_and_cli() {
        // Whitespace in env values is common when shell-export patterns
        // leave trailing newlines or quoted values pad arguments. The
        // override parser must tolerate this so `RUNNER_PM=" pnpm "`
        // works the same as `RUNNER_PM=pnpm` instead of erroring on an
        // "unknown package manager" with the padded label.
        let from_env = ResolutionOverrides::from_sources(OverrideSources {
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

        let from_cli = ResolutionOverrides::from_sources(OverrideSources {
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
        let blank = ResolutionOverrides::from_sources(OverrideSources {
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
    fn describe_renders_human_friendly_step_label() {
        use crate::tool::node::OnFail;

        // Table-driven: each row pairs a decision with the exact string
        // it must produce. Locks down the provenance wording that
        // `--explain` and `runner why` surface verbatim; a casual
        // re-phrase shouldn't slip through silently.
        let cases: &[(super::ResolvedPm, &str)] = &[
            (
                super::ResolvedPm {
                    pm: PackageManager::Yarn,
                    via: ResolutionStep::Override(OverrideOrigin::CliFlag),
                    warnings: vec![],
                },
                "yarn via --pm (CLI override)",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Bun,
                    via: ResolutionStep::Override(OverrideOrigin::EnvVar),
                    warnings: vec![],
                },
                "bun via RUNNER_PM (environment)",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Pnpm,
                    via: ResolutionStep::Override(OverrideOrigin::ConfigFile {
                        path: PathBuf::from("/proj/runner.toml"),
                    }),
                    warnings: vec![],
                },
                "pnpm via runner.toml at /proj/runner.toml",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Pnpm,
                    via: ResolutionStep::ManifestPackageManager,
                    warnings: vec![],
                },
                "pnpm via package.json \"packageManager\"",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Bun,
                    via: ResolutionStep::ManifestDevEngines {
                        on_fail: OnFail::Error,
                    },
                    warnings: vec![],
                },
                "bun via package.json \"devEngines.packageManager\" (onFail=Error)",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Pnpm,
                    via: ResolutionStep::Lockfile,
                    warnings: vec![],
                },
                "pnpm via detected lockfile",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Npm,
                    via: ResolutionStep::PathProbe {
                        binary: PathBuf::from("/usr/bin/npm"),
                    },
                    warnings: vec![],
                },
                "npm via PATH probe at /usr/bin/npm",
            ),
            (
                super::ResolvedPm {
                    pm: PackageManager::Npm,
                    via: ResolutionStep::LegacyNpmFallback,
                    warnings: vec![],
                },
                "npm via --fallback=npm (legacy)",
            ),
        ];

        for (decision, expected) in cases {
            assert_eq!(&decision.describe(), expected);
        }
    }

    #[test]
    fn deno_config_value_lands_under_deno_ecosystem_and_resolves_for_node_scripts() {
        // The runner.toml field is `[pm].node = "deno"`; the resolver
        // stores it under Ecosystem::Deno (per PackageManager::ecosystem)
        // and the Node-script resolver consults both Node and Deno keys.
        let loaded = loaded_config_with_node("deno");
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
            config: Some(&loaded),
            ..OverrideSources::default()
        })
        .expect("deno config should parse");

        assert!(overrides.pm_by_ecosystem.contains_key(&Ecosystem::Deno));

        let ctx = context(vec![PackageManager::Pnpm]);
        let decision = Resolver::new(&ctx, &overrides)
            .resolve_node_pm()
            .expect("resolution should succeed");
        assert_eq!(decision.pm, PackageManager::Deno);
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let overrides = ResolutionOverrides::from_sources(OverrideSources {
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
        let err = ResolutionOverrides::from_sources(OverrideSources {
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
