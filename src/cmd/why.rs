//! `runner why <task>` — explain how a specific task name would be
//! dispatched.
//!
//! Walks the same source-selection chain used by `runner run`, plus the PM
//! resolution chain when a `package.json` script is in the candidate set,
//! and reports what would happen step by step. Pairs with `runner doctor`
//! (project-wide diagnostic) and `--explain` (one-line trace at run time).

use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;
use serde_json::{Value, json};

use crate::cmd::run::{select_task_entry, source_depth, source_priority};
use crate::resolver::{ResolutionOverrides, Resolver};
use crate::types::{ProjectContext, Task, TaskSource};

/// Explain how `task` would resolve in the current project.
///
/// # Errors
///
/// Propagates `Resolver::resolve_node_pm` errors when a `package.json`
/// candidate would have been selected and the fallback policy is
/// `error`.
pub(crate) fn why(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    json: bool,
) -> Result<()> {
    let candidates: Vec<&Task> = ctx.tasks.iter().filter(|t| t.name == task).collect();

    let selected = (!candidates.is_empty()).then(|| select_task_entry(ctx, overrides, &candidates));

    let pm_decision = if selected
        .as_ref()
        .is_some_and(|t| t.source == TaskSource::PackageJson)
    {
        Some(Resolver::new(ctx, overrides).resolve_node_pm())
    } else {
        None
    };

    let report = build_report(
        task,
        &candidates,
        selected,
        pm_decision.as_ref(),
        overrides,
        ctx,
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(
            task,
            &candidates,
            selected,
            pm_decision.as_ref(),
            overrides,
            ctx,
        );
    }

    Ok(())
}

fn build_report(
    task: &str,
    candidates: &[&Task],
    selected: Option<&Task>,
    pm_decision: Option<&Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>>,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
) -> Value {
    json!({
        "schema_version": 1,
        "task": task,
        "candidates": candidates.iter().map(|c| candidate_json(c, overrides, ctx)).collect::<Vec<_>>(),
        "selected": selected.map(|s| candidate_json(s, overrides, ctx)),
        "pm_resolution": pm_decision.map(|res| match res {
            Ok(decision) => json!({
                "pm": decision.pm.label(),
                "via": decision.describe(),
                "warnings": decision.warnings.iter().map(|w| json!({
                    "source": w.source(),
                    "detail": w.detail(),
                })).collect::<Vec<_>>(),
            }),
            Err(err) => json!({ "error": format!("{err}") }),
        }),
    })
}

fn candidate_json(task: &Task, overrides: &ResolutionOverrides, ctx: &ProjectContext) -> Value {
    let depth = source_depth(ctx, task.source);
    let depth_value = if depth == usize::MAX {
        Value::Null
    } else {
        json!(depth)
    };
    json!({
        "source": task.source.label(),
        "source_priority": source_priority(overrides, task.source),
        "depth": depth_value,
        "display_order": task.source.display_order(),
        "is_alias": task.alias_of.is_some(),
        "alias_of": task.alias_of,
        "description": task.description,
        "passthrough_to": task.passthrough_to.map(crate::types::TaskRunner::label),
        "source_dir": source_dir_for_task(task, ctx).map(|p| p.display().to_string()),
    })
}

/// Locate the filesystem path that anchors a task's source by searching from the project root.
///
/// Returns the file or directory path associated with `task.source`, or `None` if no anchor
/// (manifest/config file or other source-specific file) is found.
///
/// # Examples
///
/// ```no_run
/// let ctx = ProjectContext { root: std::path::PathBuf::from("/path/to/project"), ..Default::default() };
/// let task = Task { source: TaskSource::PackageJson, ..Default::default() };
/// let anchor = source_dir_for_task(&task, &ctx);
/// assert!(anchor.is_none() || anchor.unwrap().is_absolute());
/// ```
fn source_dir_for_task(task: &Task, ctx: &ProjectContext) -> Option<PathBuf> {
    use crate::tool;

    match task.source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(&ctx.root),
        TaskSource::DenoJson => tool::deno::find_config_upwards(&ctx.root),
        TaskSource::TurboJson => tool::turbo::find_config(&ctx.root),
        TaskSource::Makefile => tool::files::find_first(&ctx.root, tool::make::FILENAMES),
        TaskSource::Justfile => tool::just::find_file(&ctx.root),
        TaskSource::Taskfile => tool::files::find_first(&ctx.root, tool::go_task::FILENAMES),
        TaskSource::CargoAliases => tool::cargo_aliases::find_anchor(&ctx.root),
        TaskSource::BaconToml => tool::files::find_first(&ctx.root, tool::bacon::FILENAMES),
        TaskSource::MiseToml => tool::mise::find_file(&ctx.root),
    }
}

fn print_human(
    task: &str,
    candidates: &[&Task],
    selected: Option<&Task>,
    pm_decision: Option<&Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>>,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
) {
    println!("{} {}", "runner why".bold(), task.bold());
    println!();

    if candidates.is_empty() {
        println!(
            "  {}",
            "No task with that name in any detected source.".dimmed()
        );
        println!(
            "  {}",
            "Without a match, `runner run` would treat it as a command and route through the \
             primary PM's exec primitive (npx-style)."
                .dimmed()
        );
        return;
    }

    println!("{}", "Candidates".bold());
    for c in candidates {
        let depth = source_depth(ctx, c.source);
        let depth_label = if depth == usize::MAX {
            "—".to_string()
        } else {
            depth.to_string()
        };
        let alias_tag = c
            .alias_of
            .as_deref()
            .map_or(String::new(), |target| format!(" → {target}"));
        let passthrough_tag = c.passthrough_to.map_or(String::new(), |r| {
            format!(" (passthrough to {})", r.label())
        });
        println!(
            "  {} {} [priority={}, depth={}, order={}]{}{}",
            "·".dimmed(),
            c.source.label().bold(),
            source_priority(overrides, c.source),
            depth_label,
            c.source.display_order(),
            alias_tag,
            passthrough_tag,
        );
    }
    println!();

    if let Some(sel) = selected {
        println!(
            "{} {} {}",
            "Selected".bold(),
            "→".dimmed(),
            sel.source.label().green()
        );
        println!(
            "  {}",
            "key: (source_priority, depth, display_order, alias_last)".dimmed()
        );
    }

    if let Some(res) = pm_decision {
        println!();
        println!("{}", "PM resolution".bold());
        match res {
            Ok(decision) => {
                println!("  {}", decision.describe());
                for w in &decision.warnings {
                    println!("  {} {w}", "warn:".yellow().bold());
                }
            }
            Err(err) => {
                println!("  {} {err}", "error:".red().bold());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::why;
    use crate::resolver::ResolutionOverrides;
    use crate::types::{ProjectContext, Task, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("/tmp/test"),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn why_handles_missing_task() {
        let ctx = context(vec![]);
        why(&ctx, &ResolutionOverrides::default(), "build", true)
            .expect("why should succeed even when task is missing");
    }

    #[test]
    fn why_with_multiple_candidates_renders_both_formats() {
        let ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::Justfile),
        ]);
        why(&ctx, &ResolutionOverrides::default(), "build", true).expect("json should succeed");
        why(&ctx, &ResolutionOverrides::default(), "build", false).expect("human should succeed");
    }
}
