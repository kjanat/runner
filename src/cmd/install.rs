//! `runner install` — install dependencies via every detected package manager.

use std::any::Any;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};
use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskRunner, version_matches};

/// Install dependencies for each detected package manager.
///
/// Warns when the current Node.js version doesn't match the project's
/// expected version before proceeding. Thin wrapper over [`install_pms`]
/// that preserves the package manager's actual exit code for callers.
pub(crate) fn install(ctx: &ProjectContext, frozen: bool) -> Result<i32> {
    install_pms(ctx, frozen, None)
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
    frozen: bool,
    _sink: super::WarningSink<'_>,
) -> Result<i32> {
    if ctx.package_managers.is_empty() {
        bail!("No package manager detected.");
    }

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

    if ctx.package_managers.len() == 1 {
        let pm = ctx.package_managers[0];
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
        let mut cmd = build_install_command(ctx, pm, frozen);
        super::configure_command(&mut cmd, &ctx.root);
        let status = cmd.status()?;
        return Ok(if status.success() {
            0
        } else {
            super::exit_code(status)
        });
    }

    run_installs_parallel(ctx, frozen)
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
fn run_installs_parallel(ctx: &ProjectContext, frozen: bool) -> Result<i32> {
    use std::process::Child;

    let names: Vec<&str> = ctx.package_managers.iter().map(|pm| pm.label()).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);

    let mut children: Vec<(PackageManager, Child)> = Vec::with_capacity(ctx.package_managers.len());
    let mut reader_handles = Vec::new();

    let spawn_outcome: Result<()> = (|| {
        for pm in &ctx.package_managers {
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
