//! `runner install` — install dependencies via every detected package manager.

use std::any::Any;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};
use crate::resolver::{ResolutionOverrides, ResolveError, ScriptPolicy};
use crate::tool;
use crate::types::{DetectionWarning, PackageManager, ProjectContext, TaskRunner, version_matches};

/// Install dependencies for each detected package manager.
///
/// Warns when the current Node.js version doesn't match the project's
/// expected version before proceeding. Thin wrapper over [`install_pms`]
/// that preserves the package manager's actual exit code for callers.
pub(crate) fn install(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    frozen: bool,
) -> Result<i32> {
    install_pms(ctx, overrides, frozen, None)
}

/// Chain-aware install entry. Runs install across every detected PM and
/// returns the first failing PM's exit code, or 0 if all succeed.
///
/// `_sink` is accepted for chain-mode parity with `cmd::run::run`; today
/// install dispatch doesn't emit detection warnings of its own (those
/// flow through the resolver path), so the sink is unused. Kept on the
/// signature so future warning-emitting install paths slot in without a
/// breaking change.
///
/// Used by `chain::exec` when `ChainItemKind::Install` appears as a
/// chain item (i.e. `runner install <tasks>`).
pub(crate) fn install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    frozen: bool,
    _sink: super::WarningSink<'_>,
) -> Result<i32> {
    if ctx.package_managers.is_empty() {
        bail!("No package manager detected.");
    }

    // Resolved before the GHA group opens so a refused override doesn't
    // emit an empty `runner: install` group — same rationale as the
    // no-PM bail above.
    let pms = select_install_pms(ctx, overrides)?;

    warn_selected_collisions(ctx, &pms, overrides);
    warn_unsupported_script_policy(&pms, overrides);

    // Collapse the whole install (single- or multi-PM) under one
    // `runner: install` GitHub Actions group when enabled.
    let _group = super::task_group(overrides, "install");

    if let (Some(nv), Some(cur)) = (&ctx.node_version, &ctx.current_node)
        && !version_matches(&nv.expected, cur)
    {
        eprintln!(
            "{} node expected {} ({}), current {}",
            "warn:".yellow().bold(),
            nv.expected,
            nv.source,
            cur,
        );
        suggest_version_switch(ctx);
    }

    if let [pm] = pms.as_slice() {
        return install_single(ctx, *pm, frozen, overrides);
    }

    run_installs_parallel(ctx, &pms, frozen, overrides)
}

/// Which PMs this invocation installs with, in precedence order:
///
/// 1. The cross-ecosystem `--pm`/`RUNNER_PM` override (which also affects
///    script dispatch) — installs with that PM alone; errors if it isn't
///    detected.
/// 2. The install-scoped allowlist `RUNNER_INSTALL_PMS` / `[install].pms`
///    (resolved into `overrides.install_pms`) — installs with the detected
///    PMs in that list, preserving detection order; errors if a listed PM
///    isn't detected.
/// 3. Otherwise every detected PM.
///
/// `pm_by_ecosystem` (runner.toml `[pm].node`/`[pm].python`) is
/// deliberately NOT consulted: it scopes *script dispatch* to an
/// ecosystem. The `[install]` allowlist is the install-fan-out knob.
fn select_install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Result<Vec<PackageManager>, ResolveError> {
    if let Some(o) = &overrides.pm {
        return if ctx.package_managers.contains(&o.pm) {
            Ok(vec![o.pm])
        } else {
            Err(ResolveError::PmOverrideNotDetected {
                pm: o.pm,
                origin: o.origin.clone(),
                detected: ctx.package_managers.clone(),
            })
        };
    }

    if !overrides.install_pms.is_empty() {
        let missing: Vec<PackageManager> = overrides
            .install_pms
            .iter()
            .copied()
            .filter(|pm| !ctx.package_managers.contains(pm))
            .collect();
        if !missing.is_empty() {
            return Err(ResolveError::InstallPmsNotDetected {
                missing,
                detected: ctx.package_managers.clone(),
            });
        }
        // Keep detection order; the allowlist only filters, never reorders.
        return Ok(ctx
            .package_managers
            .iter()
            .copied()
            .filter(|pm| overrides.install_pms.contains(pm))
            .collect());
    }

    Ok(ctx.package_managers.clone())
}

/// Warn about an install-dir collision only when the *selected* PM set
/// still collides. Detection records the collision over every detected PM
/// (so `doctor` reports it), but `[install].pms` / `RUNNER_INSTALL_PMS` may
/// already have narrowed install to a single writer — in which case there
/// is nothing left to warn about.
fn warn_selected_collisions(
    ctx: &ProjectContext,
    pms: &[PackageManager],
    overrides: &ResolutionOverrides,
) {
    if overrides.no_warnings {
        return;
    }
    for warning in &ctx.warnings {
        if let Some(reduced) = still_colliding(warning, pms) {
            eprintln!("{} {reduced}", "warn:".yellow().bold());
        }
    }
}

/// If `warning` is an install-dir collision that still has ≥2 writers once
/// narrowed to the `selected` install set, return the reduced warning to
/// emit; otherwise `None`. Pure so the narrowing is unit-testable.
fn still_colliding(
    warning: &DetectionWarning,
    selected: &[PackageManager],
) -> Option<DetectionWarning> {
    let DetectionWarning::InstallDirCollision { dir, pms: writers } = warning else {
        return None;
    };
    // Copy the `&'static str` out of the by-ref match binding (`&&str`).
    let &dir = dir;
    let still: Vec<PackageManager> = writers
        .iter()
        .copied()
        .filter(|pm| selected.contains(pm))
        .collect();
    (still.len() >= 2).then_some(DetectionWarning::InstallDirCollision { dir, pms: still })
}

/// Run a single PM's install in the foreground, inheriting stdio.
fn install_single(
    ctx: &ProjectContext,
    pm: PackageManager,
    frozen: bool,
    overrides: &ResolutionOverrides,
) -> Result<i32> {
    eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
    let mut cmd = build_install_command(ctx, pm, frozen, deny_scripts(overrides));
    super::configure_command(&mut cmd, &ctx.root, overrides);
    let status = cmd.status()?;
    Ok(if status.success() {
        0
    } else {
        super::exit_code(status)
    })
}

/// Run every detected package manager's install in parallel, multiplexing
/// stdout/stderr through a [`LineSink`] so each line is prefixed with the
/// PM that produced it.
///
/// Failure policy mirrors chain mode's `FailFast` default: record the
/// first non-zero exit code, let the remaining installs finish on their
/// own. Killing siblings on first failure (the `KillOnFail` analogue)
/// isn't exposed yet — the v1 `runner install` CLI has no flag for it,
/// and the conservative default for a top-level command is "don't tear
/// down the user's slow `cargo fetch` because `npm` blew up on a 404."
fn run_installs_parallel(
    ctx: &ProjectContext,
    pms: &[PackageManager],
    frozen: bool,
    overrides: &ResolutionOverrides,
) -> Result<i32> {
    use std::process::Child;

    let names: Vec<&str> = pms.iter().map(|pm| pm.label()).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);
    let deny = deny_scripts(overrides);

    let mut children: Vec<(PackageManager, Child)> = Vec::with_capacity(pms.len());
    let mut reader_handles = Vec::new();

    let spawn_outcome: Result<()> = (|| {
        for pm in pms {
            eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
            let mut cmd = build_install_command(ctx, *pm, frozen, deny);
            super::configure_command(&mut cmd, &ctx.root, overrides);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn()?;
            let prefix = render_prefix(pm.label(), width, colorize);
            let stdout: Box<dyn std::io::Read + Send> =
                Box::new(child.stdout.take().expect("stdout piped"));
            let stderr: Box<dyn std::io::Read + Send> =
                Box::new(child.stderr.take().expect("stderr piped"));
            reader_handles.extend(spawn_readers(
                vec![
                    (prefix.clone(), false, stdout),
                    (prefix.clone(), true, stderr),
                ],
                &sink,
            ));
            children.push((*pm, child));
        }
        Ok(())
    })();
    if let Err(e) = spawn_outcome {
        for (_, mut c) in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        for h in reader_handles {
            join_reader_thread(h);
        }
        return Err(e);
    }

    let mut first_failure: Option<i32> = None;
    for (_, mut child) in children {
        let status = child.wait()?;
        if !status.success() {
            first_failure.get_or_insert_with(|| super::exit_code(status));
        }
    }
    for h in reader_handles {
        join_reader_thread(h);
    }

    Ok(first_failure.unwrap_or(0))
}

fn join_reader_thread(handle: JoinHandle<()>) {
    if let Err(payload) = handle.join() {
        eprintln!(
            "warn: install output reader thread panicked: {}",
            panic_payload(&*payload),
        );
    }
}

fn panic_payload(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

/// Map a [`PackageManager`] to its install [`Command`].
///
/// `deny_scripts` requests that lifecycle scripts be skipped; it is honored
/// only by the package managers that expose a skip mechanism (npm, yarn,
/// pnpm, bun, composer). deno already denies dependency scripts by default,
/// and the remaining managers have no toggle — the caller
/// ([`warn_unsupported_script_policy`]) warns about those before reaching here.
fn build_install_command(
    ctx: &ProjectContext,
    pm: PackageManager,
    frozen: bool,
    deny_scripts: bool,
) -> Command {
    match pm {
        PackageManager::Npm => tool::npm::install_cmd(frozen, deny_scripts),
        PackageManager::Yarn => tool::yarn::install_cmd(&ctx.root, frozen, deny_scripts),
        PackageManager::Pnpm => tool::pnpm::install_cmd(frozen, deny_scripts),
        PackageManager::Bun => tool::bun::install_cmd(frozen, deny_scripts),
        PackageManager::Cargo => tool::cargo_pm::install_cmd(frozen),
        PackageManager::Deno => tool::deno::install_cmd(),
        PackageManager::Uv => tool::uv::install_cmd(frozen),
        PackageManager::Poetry => tool::poetry::install_cmd(),
        PackageManager::Pipenv => tool::pipenv::install_cmd(frozen),
        PackageManager::Go => tool::go_pm::install_cmd(),
        PackageManager::Bundler => tool::bundler::install_cmd(),
        PackageManager::Composer => tool::composer::install_cmd(deny_scripts),
    }
}

/// Whether the resolved [`ScriptPolicy`] asks to skip install scripts.
const fn deny_scripts(overrides: &ResolutionOverrides) -> bool {
    matches!(overrides.script_policy, ScriptPolicy::Deny)
}

/// How a package manager can honor an install-script *deny* policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenySupport {
    /// Skips scripts via a flag/env that [`build_install_command`] applies
    /// (npm/yarn/pnpm/bun `--ignore-scripts`, composer `--no-scripts`,
    /// yarn-berry `YARN_ENABLE_SCRIPTS=false`).
    ViaFlag,
    /// Already blocks dependency scripts by default (deno), so a deny
    /// request is satisfied without changing the command.
    DefaultDeny,
    /// No mechanism to skip install/build scripts (cargo `build.rs`, go,
    /// bundler native extensions, the Python build backends uv/poetry/pipenv).
    /// A deny request is reported and the install proceeds unchanged.
    Unsupported,
}

/// Classify how each package manager honors a deny-scripts policy. Exhaustive
/// over [`PackageManager`] so a newly added PM is a compile error to triage.
const fn deny_support(pm: PackageManager) -> DenySupport {
    match pm {
        PackageManager::Npm
        | PackageManager::Yarn
        | PackageManager::Pnpm
        | PackageManager::Bun
        | PackageManager::Composer => DenySupport::ViaFlag,
        PackageManager::Deno => DenySupport::DefaultDeny,
        PackageManager::Cargo
        | PackageManager::Uv
        | PackageManager::Poetry
        | PackageManager::Pipenv
        | PackageManager::Go
        | PackageManager::Bundler => DenySupport::Unsupported,
    }
}

/// Warn once per selected package manager that cannot honor a requested
/// deny-scripts policy, so a `--no-scripts` (or `[install].scripts = "deny"` /
/// `RUNNER_INSTALL_SCRIPTS=deny`) that some managers ignore is never silently
/// dropped. No-op unless the policy is [`ScriptPolicy::Deny`]; respects
/// `--no-warnings`, matching the sibling collision/version warnings.
fn warn_unsupported_script_policy(pms: &[PackageManager], overrides: &ResolutionOverrides) {
    if overrides.no_warnings || !deny_scripts(overrides) {
        return;
    }
    for pm in pms {
        if deny_support(*pm) == DenySupport::Unsupported {
            eprintln!(
                "{} {} cannot skip install scripts; deny policy not applied to it",
                "warn:".yellow().bold(),
                pm.label(),
            );
        }
    }
}

/// Print a hint about which version manager command to run.
fn suggest_version_switch(ctx: &ProjectContext) {
    let hint = if ctx
        .node_version
        .as_ref()
        .is_some_and(|nv| nv.source == ".nvmrc")
    {
        "nvm use"
    } else if ctx.task_runners.contains(&TaskRunner::Mise) {
        "mise install"
    } else {
        "switch to the expected Node version"
    };
    eprintln!("       {} {}", "hint:".dimmed(), hint);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::{
        DenySupport, build_install_command, deny_support, select_install_pms,
        warn_unsupported_script_policy,
    };
    use crate::resolver::{
        OverrideOrigin, PmOverride, ResolutionOverrides, ResolveError, ScriptPolicy,
    };
    use crate::types::{Ecosystem, PackageManager, ProjectContext};

    fn context(pms: Vec<PackageManager>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("/tmp/test"),
            package_managers: pms,
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn override_pm(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..Default::default()
        }
    }

    #[test]
    fn no_override_installs_with_every_detected_pm() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let pms = select_install_pms(&ctx, &ResolutionOverrides::default())
            .expect("default selection should succeed");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Deno]);
    }

    #[test]
    fn detected_override_installs_with_it_alone() {
        // The dreamcli CI bug: bun + deno detected, RUNNER_PM=bun set —
        // deno must not install (and must not write deno.lock).
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let overrides = override_pm(PackageManager::Bun, OverrideOrigin::EnvVar);
        let pms = select_install_pms(&ctx, &overrides).expect("detected override should filter");
        assert_eq!(pms, vec![PackageManager::Bun]);
    }

    #[test]
    fn undetected_override_errors_with_origin_and_detected_list() {
        let ctx = context(vec![PackageManager::Cargo]);
        let overrides = override_pm(PackageManager::Npm, OverrideOrigin::EnvVar);
        let err = select_install_pms(&ctx, &overrides).expect_err("undetected override must error");

        assert!(matches!(err, ResolveError::PmOverrideNotDetected { .. }));
        let msg = format!("{err}");
        assert!(msg.contains("RUNNER_PM"), "should name the source: {msg}");
        assert!(msg.contains("cargo"), "should list detected PMs: {msg}");
    }

    #[test]
    fn undetected_cli_override_names_the_flag() {
        let ctx = context(vec![PackageManager::Cargo]);
        let overrides = override_pm(PackageManager::Npm, OverrideOrigin::CliFlag);
        let err = select_install_pms(&ctx, &overrides).expect_err("undetected override must error");

        let msg = format!("{err}");
        assert!(msg.contains("--pm"), "should name the flag: {msg}");
    }

    #[test]
    fn still_colliding_clears_once_filtered_to_single_writer() {
        use super::still_colliding;
        use crate::types::DetectionWarning;

        let warning = DetectionWarning::InstallDirCollision {
            dir: "node_modules",
            pms: vec![PackageManager::Bun, PackageManager::Deno],
        };
        // Restricting install to bun alone resolves the collision.
        assert!(still_colliding(&warning, &[PackageManager::Bun]).is_none());
        // Both still selected → still a collision (reduced set unchanged).
        let reduced = still_colliding(&warning, &[PackageManager::Bun, PackageManager::Deno])
            .expect("two writers still collide");
        match reduced {
            DetectionWarning::InstallDirCollision { dir, pms } => {
                assert_eq!(dir, "node_modules");
                assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Deno]);
            }
            other => panic!("expected collision, got {other:?}"),
        }
    }

    #[test]
    fn install_pms_allowlist_filters_to_listed_detected_pms() {
        // The reported case: bun + deno + cargo detected, allowlist = [bun]
        // → only bun installs (no competing node_modules writer, no cargo).
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Deno,
            PackageManager::Cargo,
        ]);
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Bun],
            ..Default::default()
        };
        let pms = select_install_pms(&ctx, &overrides).expect("allowlist should filter");
        assert_eq!(pms, vec![PackageManager::Bun]);
    }

    #[test]
    fn install_pms_allowlist_preserves_detection_order() {
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Cargo,
            PackageManager::Uv,
        ]);
        // Listed out of detection order — output still follows detection.
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Uv, PackageManager::Bun],
            ..Default::default()
        };
        let pms = select_install_pms(&ctx, &overrides).expect("allowlist should filter");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Uv]);
    }

    #[test]
    fn install_pms_undetected_entry_errors() {
        let ctx = context(vec![PackageManager::Bun]);
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Bun, PackageManager::Pnpm],
            ..Default::default()
        };
        let err = select_install_pms(&ctx, &overrides).expect_err("undetected entry must error");
        assert!(matches!(err, ResolveError::InstallPmsNotDetected { .. }));
        let msg = format!("{err}");
        assert!(msg.contains("pnpm"), "names the missing PM: {msg}");
        assert!(msg.contains("bun"), "lists detected: {msg}");
    }

    #[test]
    fn pm_override_wins_over_install_pms_allowlist() {
        // --pm/RUNNER_PM is the cross-ecosystem override; it takes
        // precedence and the install allowlist is not consulted.
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let mut overrides = override_pm(PackageManager::Deno, OverrideOrigin::EnvVar);
        overrides.install_pms = vec![PackageManager::Bun];
        let pms = select_install_pms(&ctx, &overrides).expect("override wins");
        assert_eq!(pms, vec![PackageManager::Deno]);
    }

    #[test]
    fn empty_install_pms_installs_with_every_detected_pm() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Cargo]);
        let pms = select_install_pms(&ctx, &ResolutionOverrides::default())
            .expect("no allowlist installs all");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Cargo]);
    }

    #[test]
    fn ecosystem_config_override_does_not_filter_installs() {
        // Pins the documented non-goal: `[pm].node` in runner.toml scopes
        // script dispatch, not the install set.
        let ctx = context(vec![PackageManager::Bun, PackageManager::Cargo]);
        let mut pm_by_ecosystem = HashMap::new();
        pm_by_ecosystem.insert(
            Ecosystem::Node,
            PmOverride {
                pm: PackageManager::Pnpm,
                origin: OverrideOrigin::ConfigFile {
                    path: PathBuf::from("/tmp/test/runner.toml"),
                },
            },
        );
        let overrides = ResolutionOverrides {
            pm_by_ecosystem,
            ..Default::default()
        };

        let pms = select_install_pms(&ctx, &overrides).expect("config must not filter installs");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Cargo]);
    }

    /// argv produced by `build_install_command` for `pm`, ignoring the
    /// subprocess-probing managers (yarn) whose flag logic is unit-tested in
    /// their own module.
    fn install_argv(pm: PackageManager, deny: bool) -> Vec<String> {
        let ctx = context(vec![pm]);
        build_install_command(&ctx, pm, false, deny)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn deny_support_classifies_every_pm() {
        for pm in [
            PackageManager::Npm,
            PackageManager::Yarn,
            PackageManager::Pnpm,
            PackageManager::Bun,
            PackageManager::Composer,
        ] {
            assert_eq!(
                deny_support(pm),
                DenySupport::ViaFlag,
                "{} via flag",
                pm.label()
            );
        }
        assert_eq!(deny_support(PackageManager::Deno), DenySupport::DefaultDeny);
        for pm in [
            PackageManager::Cargo,
            PackageManager::Uv,
            PackageManager::Poetry,
            PackageManager::Pipenv,
            PackageManager::Go,
            PackageManager::Bundler,
        ] {
            assert_eq!(
                deny_support(pm),
                DenySupport::Unsupported,
                "{} unsupported",
                pm.label(),
            );
        }
    }

    #[test]
    fn deny_appends_skip_flag_for_flag_managers() {
        assert_eq!(
            install_argv(PackageManager::Npm, true),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Pnpm, true),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Bun, true),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Composer, true),
            ["install", "--no-scripts"]
        );
    }

    #[test]
    fn deny_is_noop_for_default_deny_and_unsupported_managers() {
        // deno already denies by default — no flag added.
        assert_eq!(install_argv(PackageManager::Deno, true), ["install"]);
        // cargo has no toggle — the deny is reported elsewhere, command unchanged.
        assert_eq!(install_argv(PackageManager::Cargo, true), ["fetch"]);
    }

    #[test]
    fn allow_or_default_adds_no_skip_flag() {
        assert_eq!(install_argv(PackageManager::Npm, false), ["install"]);
        assert_eq!(install_argv(PackageManager::Composer, false), ["install"]);
    }

    #[test]
    fn warn_unsupported_is_silent_unless_denying() {
        // Smoke: no panic, and the no-warnings / non-deny guards short-circuit
        // before any emission (stderr capture isn't worth a fixture here).
        let pms = [PackageManager::Cargo, PackageManager::Npm];
        warn_unsupported_script_policy(&pms, &ResolutionOverrides::default());
        let denying = ResolutionOverrides {
            script_policy: ScriptPolicy::Deny,
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        warn_unsupported_script_policy(&pms, &denying);
    }
}
