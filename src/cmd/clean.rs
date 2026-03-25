use std::io::Write as _;
use std::path::Path;
use std::{fs, io};

use anyhow::Result;
use colored::Colorize;

use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskRunner};

pub fn clean(ctx: &ProjectContext, skip_confirm: bool) -> Result<()> {
    let mut targets: Vec<&str> = Vec::new();

    for pm in &ctx.package_managers {
        let dirs: &[&str] = match pm {
            PackageManager::Npm
            | PackageManager::Yarn
            | PackageManager::Pnpm
            | PackageManager::Bun => tool::node::CLEAN_DIRS,
            PackageManager::Cargo => tool::cargo_pm::CLEAN_DIRS,
            PackageManager::Deno => tool::deno::CLEAN_DIRS,
            PackageManager::Uv => tool::uv::CLEAN_DIRS,
            PackageManager::Poetry => tool::poetry::CLEAN_DIRS,
            PackageManager::Pipenv => tool::pipenv::CLEAN_DIRS,
            PackageManager::Go => tool::go_pm::CLEAN_DIRS,
            PackageManager::Bundler | PackageManager::Composer => &[],
        };
        for d in dirs {
            push_if_exists(&mut targets, d);
        }
    }

    for tr in &ctx.task_runners {
        let dirs: &[&str] = match tr {
            TaskRunner::Turbo => tool::turbo::CLEAN_DIRS,
            TaskRunner::Nx => tool::nx::CLEAN_DIRS,
            _ => &[],
        };
        for d in dirs {
            push_if_exists(&mut targets, d);
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
