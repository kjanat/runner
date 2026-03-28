//! `runner run <task>` — resolve a task name to the right tool and execute it.

use std::process::Command;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskSource};

/// Look up `task` across all detected sources, pick the highest-priority
/// match, build the appropriate command, and execute it.
///
/// Bun special case: when `task == "test"` and no package-manifest `test`
/// script exists, falls back to `bun test`.
///
/// Returns the child process exit code.
pub(crate) fn run(ctx: &ProjectContext, task: &str, args: &[String]) -> Result<i32> {
    super::print_warnings(ctx);

    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task).collect();

    if found.is_empty() {
        if let Some(code) = run_bun_test_fallback(ctx, task, args)? {
            return Ok(code);
        }

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
    Ok(super::exit_code(cmd.status()?))
}

fn run_bun_test_fallback(ctx: &ProjectContext, task: &str, args: &[String]) -> Result<Option<i32>> {
    if !should_use_bun_test_fallback(ctx, task) {
        return Ok(None);
    }

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        "bun".dimmed(),
        "test".bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = tool::bun::test_cmd(args);
    super::configure_command(&mut cmd, &ctx.root);
    Ok(Some(super::exit_code(cmd.status()?)))
}

fn should_use_bun_test_fallback(ctx: &ProjectContext, task: &str) -> bool {
    task == "test"
        && !has_package_script(ctx, task)
        && ctx
            .primary_node_pm()
            .or_else(|| ctx.primary_pm())
            .is_some_and(|pm| pm == PackageManager::Bun)
}

fn has_package_script(ctx: &ProjectContext, task: &str) -> bool {
    ctx.tasks
        .iter()
        .any(|entry| entry.source == TaskSource::PackageJson && entry.name == task)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::should_use_bun_test_fallback;
    use crate::types::{PackageManager, ProjectContext, Task, TaskSource};

    #[test]
    fn bun_test_fallback_enabled_when_no_test_script() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(should_use_bun_test_fallback(&ctx, "test"));
    }

    #[test]
    fn bun_test_fallback_disabled_when_test_script_exists() {
        let ctx = context(
            vec![PackageManager::Bun],
            vec![Task {
                name: "test".to_string(),
                source: TaskSource::PackageJson,
                description: None,
            }],
        );

        assert!(!should_use_bun_test_fallback(&ctx, "test"));
    }

    #[test]
    fn bun_test_fallback_disabled_for_other_package_managers() {
        let ctx = context(vec![PackageManager::Npm], vec![]);

        assert!(!should_use_bun_test_fallback(&ctx, "test"));
    }

    #[test]
    fn bun_test_fallback_disabled_for_non_test_task() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(&ctx, "build"));
    }

    fn context(package_managers: Vec<PackageManager>, tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers,
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }
}
