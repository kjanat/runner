use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskSource};

pub fn run(ctx: &ProjectContext, task: &str, args: &[String]) -> Result<()> {
    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task).collect();

    if found.is_empty() {
        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    // Priority: turbo > package.json > makefile > justfile > taskfile > deno
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
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

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
