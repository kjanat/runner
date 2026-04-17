//! `runner run <target>` — resolve a task name to the right tool and execute
//! it. When no task matches, fall back to executing the target as an
//! arbitrary command through the detected package manager (formerly `runner
//! exec`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskSource};

/// Parse `"source:task"` syntax. Returns `(Some(source), task_name)` if the
/// prefix before the first `:` is a known source label, or `(None, original)`
/// for bare names and names with colons that don't match a source.
fn parse_qualified_task(input: &str) -> (Option<TaskSource>, &str) {
    if let Some(colon) = input.find(':') {
        let prefix = &input[..colon];
        if let Some(source) = TaskSource::from_label(prefix) {
            return (Some(source), &input[colon + 1..]);
        }
    }
    (None, input)
}

/// Look up `task` across all detected sources, pick the highest-priority
/// match, build the appropriate command, and execute it.
///
/// Bun special case: when `task == "test"` and no package-manifest `test`
/// script exists, falls back to `bun test`.
///
/// Returns the child process exit code.
pub(crate) fn run(ctx: &ProjectContext, task: &str, args: &[String]) -> Result<i32> {
    super::print_warnings(ctx);

    let (qualifier, task_name) = parse_qualified_task(task);

    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task_name).collect();

    if found.is_empty() {
        if let Some(code) = run_bun_test_fallback(ctx, task_name, args)? {
            return Ok(code);
        }

        if qualifier.is_none() {
            return run_pm_exec_fallback(ctx, task_name, args);
        }

        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    let entry = if let Some(source) = qualifier {
        found
            .iter()
            .find(|t| t.source == source)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("task {task_name:?} not found in {}", source.label()))?
    } else {
        select_task_entry(ctx, &found)
    };

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        entry.source.label().dimmed(),
        task_name.bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = build_run_command(ctx, entry.source, task_name, args)?;
    super::configure_command(&mut cmd, &ctx.root);
    Ok(super::exit_code(cmd.status()?))
}

fn select_task_entry<'a>(
    ctx: &ProjectContext,
    found: &[&'a crate::types::Task],
) -> &'a crate::types::Task {
    if ctx.package_managers.contains(&PackageManager::Deno) {
        return found
            .iter()
            .min_by_key(|task| deno_task_priority(ctx, task.source))
            .copied()
            .expect("task selection should have at least one match");
    }

    found
        .iter()
        .find(|t| t.source == TaskSource::TurboJson)
        .or_else(|| found.iter().find(|t| t.source == TaskSource::PackageJson))
        .or_else(|| found.first())
        .copied()
        .expect("task selection should have at least one match")
}

fn deno_task_priority(ctx: &ProjectContext, source: TaskSource) -> (usize, u8) {
    let depth = source_dir(source, &ctx.root)
        .and_then(|dir| {
            ctx.root
                .ancestors()
                .position(|ancestor| ancestor == dir.as_path())
        })
        .unwrap_or(usize::MAX);

    (depth, source.display_order())
}

fn source_dir(source: TaskSource, root: &Path) -> Option<PathBuf> {
    match source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(root),
        TaskSource::DenoJson => tool::deno::find_config_upwards(root),
        TaskSource::TurboJson => {
            let candidate = root.join(tool::turbo::FILENAME);
            candidate.is_file().then_some(candidate)
        }
        TaskSource::Makefile => tool::files::find_first(root, tool::make::FILENAMES),
        TaskSource::Justfile => tool::just::find_file(root),
        TaskSource::Taskfile => tool::files::find_first(root, tool::go_task::FILENAMES),
    }
    .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// Execute `target` (plus `args`) as an arbitrary command through the
/// detected package manager (npx, bunx, pnpm exec, cargo, uv run, etc.).
/// Falls back to running the command directly when no package manager is
/// detected.
fn run_pm_exec_fallback(ctx: &ProjectContext, target: &str, args: &[String]) -> Result<i32> {
    let mut combined = Vec::with_capacity(args.len() + 1);
    combined.push(target.to_string());
    combined.extend(args.iter().cloned());

    // Only dispatch through a PM when its exec primitive actually runs
    // arbitrary package binaries like `npx` does. For npm/yarn/pnpm/bun/uv
    // this is the whole point of `exec`. Deno, Cargo, and the Python/Ruby/
    // Go/PHP PMs have no such primitive:
    //   * Deno's `deno run <target>` treats `target` as a local script.
    //   * Cargo's `cargo <target>` dispatches to a cargo subcommand/plugin,
    //     not a binary on PATH (so `runner run eslint` in a Rust repo
    //     would try to invoke `cargo-eslint`).
    //   * Poetry/Pipenv/Bundler/Composer/Go have nothing equivalent.
    // For those we fall through to spawning `target` directly so PATH is
    // authoritative rather than silently doing the wrong thing.
    let (label, mut cmd) = match ctx.primary_pm() {
        Some(PackageManager::Npm) => ("npm", tool::npm::exec_cmd(&combined)),
        Some(PackageManager::Yarn) => ("yarn", tool::yarn::exec_cmd(&combined)),
        Some(PackageManager::Pnpm) => ("pnpm", tool::pnpm::exec_cmd(&combined)),
        Some(PackageManager::Bun) => ("bun", tool::bun::exec_cmd(&combined)),
        Some(PackageManager::Uv) => ("uv", tool::uv::exec_cmd(&combined)),
        None | Some(_) => {
            let mut c = Command::new(target);
            c.args(args);
            ("exec", c)
        }
    };

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        label.dimmed(),
        target.bold(),
        args.join(" ").dimmed(),
    );

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
    use std::fs;
    use std::path::PathBuf;

    use super::{parse_qualified_task, select_task_entry, should_use_bun_test_fallback};
    use crate::tool::test_support::TempDir;
    use crate::types::{PackageManager, ProjectContext, Task, TaskSource};

    #[test]
    fn parse_qualified_task_splits_source_and_name() {
        let (source, name) = parse_qualified_task("justfile:fmt");
        assert_eq!(source, Some(TaskSource::Justfile));
        assert_eq!(name, "fmt");
    }

    #[test]
    fn parse_qualified_task_returns_bare_name() {
        let (source, name) = parse_qualified_task("build");
        assert_eq!(source, None);
        assert_eq!(name, "build");
    }

    #[test]
    fn parse_qualified_task_handles_unknown_source() {
        let (source, name) = parse_qualified_task("unknown:build");
        assert_eq!(source, None);
        assert_eq!(name, "unknown:build");
    }

    #[test]
    fn parse_qualified_task_with_colons_in_task_name() {
        let (source, name) = parse_qualified_task("package.json:helix:sync");
        assert_eq!(source, Some(TaskSource::PackageJson));
        assert_eq!(name, "helix:sync");
    }

    #[test]
    fn parse_qualified_task_preserves_colons_in_bare_name() {
        let (source, name) = parse_qualified_task("helix:sync");
        assert_eq!(source, None);
        assert_eq!(name, "helix:sync");
    }

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

    #[test]
    fn select_task_entry_prefers_nearest_deno_source() {
        let dir = TempDir::new("run-deno-nearest");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ tasks: { build: "deno task build" } }"#,
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{ "scripts": { "build": "deno task build" } }"#,
        )
        .expect("member package.json should be written");
        let ctx = ProjectContext {
            root: nested,
            package_managers: vec![PackageManager::Deno],
            task_runners: Vec::new(),
            tasks: vec![
                Task {
                    name: "build".to_string(),
                    source: TaskSource::DenoJson,
                    description: None,
                },
                Task {
                    name: "build".to_string(),
                    source: TaskSource::PackageJson,
                    description: None,
                },
            ],
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let found: Vec<_> = ctx.tasks.iter().collect();
        let entry = select_task_entry(&ctx, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
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
