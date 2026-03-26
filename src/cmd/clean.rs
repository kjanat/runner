//! `runner clean` — remove caches and build artifacts for detected tools.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;
use std::{fs, io};

use anyhow::Result;
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskRunner};

/// Collect ecosystem-specific directories that exist under the project root,
/// prompt for confirmation (unless `skip_confirm`), then delete them.
pub(crate) fn clean(
    ctx: &ProjectContext,
    skip_confirm: bool,
    include_framework: bool,
) -> Result<()> {
    let targets = collect_targets(ctx, include_framework);

    if targets.is_empty() {
        println!("{}", "Nothing to clean.".dimmed());
        return Ok(());
    }

    println!("Will remove:");
    for t in &targets {
        println!("  {t}");
    }

    if !skip_confirm {
        print!("\nProceed? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    for t in &targets {
        let path = ctx.root.join(t);
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match result {
            Ok(()) => println!("  {} {}", "removed".red(), t),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

fn collect_targets(ctx: &ProjectContext, include_framework: bool) -> Vec<&'static str> {
    let mut targets: Vec<&'static str> = Vec::new();
    let mut seen = HashSet::new();

    for pm in &ctx.package_managers {
        match pm {
            PackageManager::Npm
            | PackageManager::Yarn
            | PackageManager::Pnpm
            | PackageManager::Bun => {
                push_dirs_if_exist(
                    &mut targets,
                    &mut seen,
                    tool::node::DEFAULT_CLEAN_DIRS,
                    &ctx.root,
                );
                if include_framework {
                    push_dirs_if_exist(
                        &mut targets,
                        &mut seen,
                        tool::node::FRAMEWORK_CLEAN_DIRS,
                        &ctx.root,
                    );
                }
            }
            PackageManager::Cargo => {
                push_dirs_if_exist(
                    &mut targets,
                    &mut seen,
                    tool::cargo_pm::CLEAN_DIRS,
                    &ctx.root,
                );
            }
            PackageManager::Deno => {
                push_dirs_if_exist(&mut targets, &mut seen, tool::deno::CLEAN_DIRS, &ctx.root);
            }
            PackageManager::Uv | PackageManager::Poetry | PackageManager::Pipenv => {
                push_dirs_if_exist(&mut targets, &mut seen, tool::python::CLEAN_DIRS, &ctx.root);
            }
            PackageManager::Go => {
                push_dirs_if_exist(&mut targets, &mut seen, tool::go_pm::CLEAN_DIRS, &ctx.root);
            }
            PackageManager::Bundler | PackageManager::Composer => {}
        }
    }

    for tr in &ctx.task_runners {
        let dirs: &[&str] = match tr {
            TaskRunner::Turbo => tool::turbo::CLEAN_DIRS,
            TaskRunner::Nx => tool::nx::CLEAN_DIRS,
            _ => &[],
        };
        push_dirs_if_exist(&mut targets, &mut seen, dirs, &ctx.root);
    }

    targets.sort_unstable();
    targets
}

fn push_dirs_if_exist(
    targets: &mut Vec<&'static str>,
    seen: &mut HashSet<&'static str>,
    dirs: &[&'static str],
    root: &Path,
) {
    for dir in dirs {
        push_if_exists(targets, seen, dir, root);
    }
}

/// Append `name` to `targets` if `root/name` exists on disk.
fn push_if_exists(
    targets: &mut Vec<&'static str>,
    seen: &mut HashSet<&'static str>,
    name: &'static str,
    root: &Path,
) {
    if root.join(name).exists() && seen.insert(name) {
        targets.push(name);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::collect_targets;
    use crate::tool::test_support::TempDir;
    use crate::types::ProjectContext;
    use crate::types::{PackageManager, TaskRunner};

    fn context(root: &std::path::Path) -> ProjectContext {
        ProjectContext {
            root: root.to_path_buf(),
            package_managers: vec![PackageManager::Npm],
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn collect_targets_skips_framework_dirs_by_default() {
        let dir = TempDir::new("clean-node-default");
        fs::create_dir(dir.path().join("node_modules")).expect("node_modules should be created");
        fs::create_dir(dir.path().join(".next")).expect(".next should be created");

        let targets = collect_targets(&context(dir.path()), false);

        assert_eq!(targets, ["node_modules"]);
    }

    #[test]
    fn collect_targets_includes_framework_dirs_on_opt_in() {
        let dir = TempDir::new("clean-node-framework");
        fs::create_dir(dir.path().join("node_modules")).expect("node_modules should be created");
        fs::create_dir(dir.path().join(".next")).expect(".next should be created");

        let targets = collect_targets(&context(dir.path()), true);

        assert_eq!(targets, [".next", "node_modules"]);
    }

    #[test]
    fn collect_targets_still_includes_task_runner_dirs() {
        let dir = TempDir::new("clean-task-runner");
        fs::create_dir(dir.path().join(".turbo")).expect(".turbo should be created");

        let mut ctx = context(dir.path());
        ctx.package_managers.clear();
        ctx.task_runners = vec![TaskRunner::Turbo];

        let targets = collect_targets(&ctx, false);

        assert_eq!(targets, [".turbo"]);
    }
}
