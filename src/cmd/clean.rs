use std::io::Write as _;
use std::path::Path;
use std::{fs, io};

use anyhow::Result;
use colored::Colorize;

use crate::detect::{PackageManager, ProjectContext, TaskRunner};

pub fn clean(ctx: &ProjectContext, skip_confirm: bool) -> Result<()> {
    let mut targets: Vec<&str> = Vec::new();

    for pm in &ctx.package_managers {
        match pm {
            PackageManager::Npm
            | PackageManager::Yarn
            | PackageManager::Pnpm
            | PackageManager::Bun => {
                for d in &[
                    "node_modules",
                    ".next",
                    "dist",
                    ".cache",
                    ".parcel-cache",
                    ".svelte-kit",
                ] {
                    push_if_exists(&mut targets, d);
                }
            }
            PackageManager::Cargo => push_if_exists(&mut targets, "target"),
            PackageManager::Deno => push_if_exists(&mut targets, ".deno"),
            PackageManager::Uv | PackageManager::Poetry | PackageManager::Pipenv => {
                for d in &[".venv", "__pycache__", ".mypy_cache", ".ruff_cache"] {
                    push_if_exists(&mut targets, d);
                }
            }
            PackageManager::Go => push_if_exists(&mut targets, "vendor"),
            _ => {}
        }
    }

    for tr in &ctx.task_runners {
        match tr {
            TaskRunner::Turbo => push_if_exists(&mut targets, ".turbo"),
            TaskRunner::Nx => push_if_exists(&mut targets, ".nx"),
            _ => {}
        }
    }

    targets.sort();
    targets.dedup();

    if targets.is_empty() {
        println!("{}", "Nothing to clean.".dimmed());
        return Ok(());
    }

    println!("Will remove:");
    for t in &targets {
        println!("  {}", t);
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
            fs::remove_dir_all(&path)?;
        } else if path.is_file() {
            fs::remove_file(&path)?;
        }
        println!("  {} {}", "removed".red(), t);
    }

    Ok(())
}

fn push_if_exists<'a>(targets: &mut Vec<&'a str>, name: &'a str) {
    if Path::new(name).exists() && !targets.contains(&name) {
        targets.push(name);
    }
}
