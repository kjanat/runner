//! `runner install` — install dependencies via every detected package manager.

use std::any::Any;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};
use crate::resolver::{ResolutionOverrides, ResolveError};
use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskRunner, version_matches};

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
        return install_single(ctx, *pm, frozen);
    }

    run_installs_parallel(ctx, &pms, frozen)
}

/// Which PMs this invocation installs with: the cross-ecosystem
/// `--pm`/`RUNNER_PM` override when present (which must name a detected
/// PM), else every detected PM.
///
/// `pm_by_ecosystem` (runner.toml `[pm].node`/`[pm].python`) is
/// deliberately NOT consulted: it scopes *script dispatch* to an
/// ecosystem, and filtering a polyglot install by it is ill-defined —
/// `[pm].node = "yarn"` saying anything about whether `cargo fetch`
/// runs would be surprising in both directions.
fn select_install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Result<Vec<PackageManager>, ResolveError> {
    match &overrides.pm {
        Some(o) if ctx.package_managers.contains(&o.pm) => Ok(vec![o.pm]),
        Some(o) => Err(ResolveError::PmOverrideNotDetected {
            pm: o.pm,
            origin: o.origin.clone(),
            detected: ctx.package_managers.clone(),
        }),
        None => Ok(ctx.package_managers.clone()),
    }
}

/// Run a single PM's install in the foreground, inheriting stdio.
fn install_single(ctx: &ProjectContext, pm: PackageManager, frozen: bool) -> Result<i32> {
    eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
    let mut cmd = build_install_command(ctx, pm, frozen);
    super::configure_command(&mut cmd, &ctx.root);
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
) -> Result<i32> {
    use std::process::Child;

    let names: Vec<&str> = pms.iter().map(|pm| pm.label()).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);

    let mut children: Vec<(PackageManager, Child)> = Vec::with_capacity(pms.len());
    let mut reader_handles = Vec::new();

    let spawn_outcome: Result<()> = (|| {
        for pm in pms {
            eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
            let mut cmd = build_install_command(ctx, *pm, frozen);
            super::configure_command(&mut cmd, &ctx.root);
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
fn build_install_command(ctx: &ProjectContext, pm: PackageManager, frozen: bool) -> Command {
    match pm {
        PackageManager::Npm => tool::npm::install_cmd(frozen),
        PackageManager::Yarn => tool::yarn::install_cmd(&ctx.root, frozen),
        PackageManager::Pnpm => tool::pnpm::install_cmd(frozen),
        PackageManager::Bun => tool::bun::install_cmd(frozen),
        PackageManager::Cargo => tool::cargo_pm::install_cmd(frozen),
        PackageManager::Deno => tool::deno::install_cmd(),
        PackageManager::Uv => tool::uv::install_cmd(frozen),
        PackageManager::Poetry => tool::poetry::install_cmd(),
        PackageManager::Pipenv => tool::pipenv::install_cmd(frozen),
        PackageManager::Go => tool::go_pm::install_cmd(),
        PackageManager::Bundler => tool::bundler::install_cmd(),
        PackageManager::Composer => tool::composer::install_cmd(),
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

    use super::select_install_pms;
    use crate::resolver::{OverrideOrigin, PmOverride, ResolutionOverrides, ResolveError};
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
}
