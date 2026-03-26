//! `runner run <task>` — resolve a task name to the right tool and execute it.

use std::process::Command;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskSource};

/// Look up `task` across all detected sources, pick the highest-priority
/// match, build the appropriate command, and execute it.
///
/// Returns the child process exit code.
pub(crate) fn run(ctx: &ProjectContext, task: &str, args: &[String]) -> Result<i32> {
    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task).collect();

    if found.is_empty() {
        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    // Priority: turbo > package.json > first match (insertion order)
    let entry = found
        .iter()
        .find(|t| t.source == TaskSource::TurboJson)
        .or_else(|| found.iter().find(|t| t.source == TaskSource::PackageJson))
        .or_else(|| found.first())
        .unwrap();

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        entry.source.label().dimmed(),
        task.bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = build_run_command(ctx, entry.source, task, args)?;
    super::configure_command(&mut cmd, &ctx.root);
    let status = cmd.status()?;
    Ok(status.code().unwrap_or(1))
}

/// Build a [`Command`] for the given task source and package manager.
fn build_run_command(
    ctx: &ProjectContext,
    source: TaskSource,
    task: &str,
    args: &[String],
) -> Result<Command> {
    Ok(match source {
        TaskSource::TurboJson => tool::turbo::run_cmd(task, args),
        TaskSource::PackageJson => {
            let pm = ctx
                .primary_node_pm()
                .or_else(|| ctx.primary_pm())
                .unwrap_or(PackageManager::Npm);
            match pm {
                PackageManager::Npm => tool::npm::run_cmd(task, args),
                PackageManager::Yarn => tool::yarn::run_cmd(task, args),
                PackageManager::Pnpm => tool::pnpm::run_cmd(task, args),
                PackageManager::Bun => tool::bun::run_cmd(task, args),
                PackageManager::Deno => tool::deno::run_cmd(task, args),
                other => bail!("{} cannot run scripts", other.label()),
            }
        }
        TaskSource::Makefile => tool::make::run_cmd(task, args),
        TaskSource::Justfile => tool::just::run_cmd(task, args),
        TaskSource::Taskfile => tool::go_task::run_cmd(task, args),
        TaskSource::DenoJson => tool::deno::run_cmd(task, args),
    })
}
