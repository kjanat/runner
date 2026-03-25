//! `runner install` — install dependencies via every detected package manager.

use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskRunner, version_matches};

/// Install dependencies for each detected package manager.
///
/// Warns when the current Node.js version doesn't match the project's
/// expected version before proceeding.
pub(crate) fn install(ctx: &ProjectContext, frozen: bool) -> Result<()> {
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

    for pm in &ctx.package_managers {
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
        let mut cmd = build_install_command(*pm, frozen);
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let status = cmd.status()?;
        if !status.success() {
            bail!(
                "{} install failed (exit {})",
                pm.label(),
                status.code().unwrap_or(1)
            );
        }
    }
    Ok(())
}

/// Map a [`PackageManager`] to its install [`Command`].
fn build_install_command(pm: PackageManager, frozen: bool) -> Command {
    match pm {
        PackageManager::Npm => tool::npm::install_cmd(frozen),
        PackageManager::Yarn => tool::yarn::install_cmd(frozen),
        PackageManager::Pnpm => tool::pnpm::install_cmd(frozen),
        PackageManager::Bun => tool::bun::install_cmd(frozen),
        PackageManager::Cargo => tool::cargo_pm::install_cmd(frozen),
        PackageManager::Deno => tool::deno::install_cmd(),
        PackageManager::Uv => tool::uv::install_cmd(frozen),
        PackageManager::Poetry => tool::poetry::install_cmd(),
        PackageManager::Pipenv => tool::pipenv::install_cmd(),
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
