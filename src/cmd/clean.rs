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
        if path.is_dir() {
            match fs::remove_dir_all(&path) {
                Ok(()) => println!("  {} {}", "removed".red(), t),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        } else if path.exists() {
            eprintln!("  {} {} (not a dir)", "skipped".yellow(), t);
        }
    }

    Ok(())
}

fn collect_targets(ctx: &ProjectContext, include_framework: bool) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
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
            PackageManager::Go => {
                push_dirs_if_exist(&mut targets, &mut seen, tool::go_pm::CLEAN_DIRS, &ctx.root);
            }
            PackageManager::Uv
            | PackageManager::Poetry
            | PackageManager::Pipenv
            | PackageManager::Bundler
            | PackageManager::Composer => {}
        }
    }

    if tool::python::detect(&ctx.root) {
        push_dirs(&mut targets, &mut seen, tool::python::clean_dirs(&ctx.root));
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
    targets: &mut Vec<String>,
    seen: &mut HashSet<String>,
    dirs: &[&str],
    root: &Path,
) {
    for dir in dirs {
        push_if_exists(targets, seen, dir, root);
    }
}

fn push_dirs(targets: &mut Vec<String>, seen: &mut HashSet<String>, dirs: Vec<String>) {
    for dir in dirs {
        if seen.insert(dir.clone()) {
            targets.push(dir);
        }
    }
}

/// Append `name` to `targets` if `root/name` exists on disk.
fn push_if_exists(targets: &mut Vec<String>, seen: &mut HashSet<String>, name: &str, root: &Path) {
    if root.join(name).is_dir() {
        let name = name.to_string();
        if seen.insert(name.clone()) {
            targets.push(name);
        }
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

    #[test]
    fn collect_targets_skips_files_named_like_artifact_dirs() {
        let dir = TempDir::new("clean-file-target");
        fs::write(dir.path().join("node_modules"), "nope")
            .expect("node_modules file should be written");

        let targets = collect_targets(&context(dir.path()), false);

        assert!(targets.is_empty());
    }

    #[test]
    fn collect_targets_includes_python_artifacts_without_python_pm() {
        let dir = TempDir::new("clean-python-generic");
        fs::write(dir.path().join("requirements.txt"), "pytest\n")
            .expect("requirements.txt should be written");
        fs::create_dir(dir.path().join("dist")).expect("dist should be created");
        fs::create_dir(dir.path().join("pkg.egg-info")).expect("pkg.egg-info should be created");

        let mut ctx = context(dir.path());
        ctx.package_managers.clear();

        let targets = collect_targets(&ctx, false);

        assert_eq!(targets, ["dist", "pkg.egg-info"]);
    }
}
