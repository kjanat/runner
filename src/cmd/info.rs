use anyhow::Result;
use colored::Colorize;

use super::list::print_tasks_grouped;
use crate::types::{ProjectContext, version_matches};

pub fn info(ctx: &ProjectContext) -> Result<()> {
    println!("{}", "runner".bold());
    println!();

    if ctx.package_managers.is_empty() && ctx.task_runners.is_empty() && ctx.tasks.is_empty() {
        println!("  {}", "No project detected in current directory.".dimmed());
        return Ok(());
    }

    if !ctx.package_managers.is_empty() {
        let pms: Vec<&str> = ctx.package_managers.iter().map(|pm| pm.label()).collect();
        println!("  {:<20}{}", "Package Managers".dimmed(), pms.join(", "));
    }

    if !ctx.task_runners.is_empty() {
        let trs: Vec<&str> = ctx.task_runners.iter().map(|tr| tr.label()).collect();
        println!("  {:<20}{}", "Task Runners".dimmed(), trs.join(", "));
    }

    if let Some(nv) = &ctx.node_version {
        let mut line = format!("{} ({})", nv.expected, nv.source);
        if let Some(cur) = &ctx.current_node {
            if version_matches(&nv.expected, cur) {
                line.push_str(&format!(", current {} {}", cur, "(ok)".green()));
            } else {
                line.push_str(&format!(", current {} {}", cur, "(mismatch)".red()));
            }
        }
        println!("  {:<20}{}", "Node".dimmed(), line);
    } else if let Some(cur) = &ctx.current_node {
        println!("  {:<20}{}", "Node".dimmed(), cur);
    }

    if ctx.is_monorepo {
        println!("  {:<20}{}", "Monorepo".dimmed(), "yes".green());
    }

    if !ctx.tasks.is_empty() {
        println!();
        print_tasks_grouped(ctx);
    }

    Ok(())
}
