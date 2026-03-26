//! `runner list` — display available tasks from all detected sources.

use std::collections::HashSet;

use colored::Colorize;

use crate::types::{ProjectContext, TaskSource};

/// Print tasks to stdout.
///
/// In `raw` mode, prints deduplicated task names one per line (for piping
/// into scripts or shell completions). Otherwise prints a human-readable
/// table grouped by source file.
pub(crate) fn list(ctx: &ProjectContext, raw: bool) {
    super::print_warnings(ctx);

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
}

/// Print tasks grouped by [`TaskSource`], one line per source.
pub(super) fn print_tasks_grouped(ctx: &ProjectContext) {
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
