use std::process::{Command, Stdio};

use anyhow::{Result, bail};
use colored::Colorize;

use crate::detect::{PackageManager, ProjectContext, TaskSource};

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
    let status = cmd.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn build_run_command(
    ctx: &ProjectContext,
    source: TaskSource,
    task: &str,
    extra: &[String],
) -> Result<Command> {
    let mut cmd = match source {
        TaskSource::TurboJson => {
            let mut c = Command::new("turbo");
            c.arg("run").arg(task);
            if !extra.is_empty() {
                c.arg("--").args(extra);
            }
            c
        }
        TaskSource::PackageJson => build_pm_run(ctx, task, extra)?,
        TaskSource::Makefile => {
            let mut c = Command::new("make");
            c.arg(task).args(extra);
            c
        }
        TaskSource::Justfile => {
            let mut c = Command::new("just");
            c.arg(task).args(extra);
            c
        }
        TaskSource::Taskfile => {
            let mut c = Command::new("task");
            c.arg(task).args(extra);
            c
        }
        TaskSource::DenoJson => {
            let mut c = Command::new("deno");
            c.arg("task").arg(task).args(extra);
            c
        }
    };
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(cmd)
}

fn build_pm_run(ctx: &ProjectContext, task: &str, extra: &[String]) -> Result<Command> {
    let pm = ctx
        .primary_node_pm()
        .or_else(|| ctx.primary_pm())
        .unwrap_or(PackageManager::Npm);

    let c = match pm {
        PackageManager::Npm => {
            let mut c = Command::new("npm");
            c.arg("run").arg(task);
            if !extra.is_empty() {
                c.arg("--").args(extra);
            }
            c
        }
        PackageManager::Yarn => {
            let mut c = Command::new("yarn");
            c.arg(task).args(extra);
            c
        }
        PackageManager::Pnpm => {
            let mut c = Command::new("pnpm");
            c.arg("run").arg(task);
            if !extra.is_empty() {
                c.arg("--").args(extra);
            }
            c
        }
        PackageManager::Bun => {
            let mut c = Command::new("bun");
            c.arg("run").arg(task).args(extra);
            c
        }
        PackageManager::Deno => {
            let mut c = Command::new("deno");
            c.arg("task").arg(task).args(extra);
            c
        }
        other => bail!("{} cannot run scripts", other.label()),
    };
    Ok(c)
}
