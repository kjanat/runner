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
pub(crate) fn clean(ctx: &ProjectContext, skip_confirm: bool) -> Result<()> {
    let mut targets: Vec<&str> = Vec::new();
    let mut seen = HashSet::new();

    for pm in &ctx.package_managers {
        let dirs: &[&str] = match pm {
            PackageManager::Npm
            | PackageManager::Yarn
            | PackageManager::Pnpm
            | PackageManager::Bun => tool::node::CLEAN_DIRS,
            PackageManager::Cargo => tool::cargo_pm::CLEAN_DIRS,
            PackageManager::Deno => tool::deno::CLEAN_DIRS,
            PackageManager::Uv | PackageManager::Poetry | PackageManager::Pipenv => {
                tool::python::CLEAN_DIRS
            }
            PackageManager::Go => tool::go_pm::CLEAN_DIRS,
            PackageManager::Bundler | PackageManager::Composer => &[],
        };
        for d in dirs {
            push_if_exists(&mut targets, &mut seen, d, &ctx.root);
        }
    }

    for tr in &ctx.task_runners {
        let dirs: &[&str] = match tr {
            TaskRunner::Turbo => tool::turbo::CLEAN_DIRS,
            TaskRunner::Nx => tool::nx::CLEAN_DIRS,
            _ => &[],
        };
        for d in dirs {
            push_if_exists(&mut targets, &mut seen, d, &ctx.root);
        }
    }

    targets.sort_unstable();

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

/// Append `name` to `targets` if `root/name` exists on disk.
fn push_if_exists<'a>(
    targets: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
    name: &'a str,
    root: &Path,
) {
    if root.join(name).exists() && seen.insert(name) {
        targets.push(name);
    }
}
