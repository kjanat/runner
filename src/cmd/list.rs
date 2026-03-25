use std::collections::HashSet;

use anyhow::Result;
use colored::Colorize;

use crate::detect::{ProjectContext, TaskSource};

pub fn list(ctx: &ProjectContext, raw: bool) -> Result<()> {
    if raw {
        let mut seen = HashSet::new();
        for task in &ctx.tasks {
            if seen.insert(&task.name) {
                println!("{}", task.name);
            }
        }
    } else if ctx.tasks.is_empty() {
        println!("{}", "No tasks found.".dimmed());
    } else {
        print_tasks_grouped(ctx);
    }
    Ok(())
}

pub fn print_tasks_grouped(ctx: &ProjectContext) {
    let sources = [
        TaskSource::PackageJson,
        TaskSource::TurboJson,
        TaskSource::Makefile,
        TaskSource::Justfile,
        TaskSource::Taskfile,
        TaskSource::DenoJson,
    ];
    for source in sources {
        let names: Vec<&str> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == source)
            .map(|t| t.name.as_str())
            .collect();
        if names.is_empty() {
            continue;
        }
        println!("  {:<16}{}", source.label().bold(), names.join(", "));
    }
}
