//! `runner list`, display available tasks from all detected sources.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use colored::Colorize;

use crate::render::list::{RenderMode, print_tasks_grouped_with_mode};
use crate::resolver::ResolutionOverrides;
use crate::schema::Project;
use crate::types::{ProjectContext, Task, TaskSource};

/// Print tasks to stdout.
///
/// In `raw` mode, prints deduplicated task names one per line (for piping
/// into scripts or shell completions). Otherwise prints a human-readable
/// table grouped by source file.
///
/// # Errors
///
/// Returns an error when `source` doesn't name a known [`TaskSource`],
/// or when `--json` serialization fails. The human-output path never
/// errors.
pub(crate) fn list(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    raw: bool,
    json: bool,
    source: Option<&str>,
) -> Result<()> {
    let parsed_source = match source {
        None => None,
        Some(label) => Some(TaskSource::from_label(label).ok_or_else(|| {
            let expected = expected_source_labels();
            anyhow!(
                "--source {label:?}: unknown source label (expected one of: {expected}, legacy \
                 filename forms like justfile/bacon.toml/Makefile are also accepted)",
            )
        })?),
    };

    if json {
        let view = Project::build_with_schema(ctx, overrides, false).into_list_view(parsed_source);
        crate::render::json::print(&view)?;
        return Ok(());
    }

    super::print_warnings(ctx, overrides, None);

    let filtered: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|t| parsed_source.is_none_or(|s| t.source == s))
        .collect();

    if raw {
        let mut seen = HashSet::new();
        for task in &filtered {
            let name = ctx.spelling(task);
            if seen.insert(name.to_string()) {
                println!("{name}");
            }
        }
    } else if filtered.is_empty() {
        println!("{}", "No tasks found.".dimmed());
    } else {
        // `runner list` is an explicit request for the task list,
        // always full detail, never collapse. The height-adaptive
        // compact path is reserved for the bare `runner` / `runner
        // info` glance view (see `print_tasks_grouped`).
        print_tasks_grouped_with_mode(
            &filtered,
            &ctx.root,
            ctx.current_member().map(Arc::as_ref),
            RenderMode::Rich,
        );
        print_conflicts(ctx, overrides);
    }
    Ok(())
}

fn expected_source_labels() -> String {
    TaskSource::all()
        .iter()
        .copied()
        .map(TaskSource::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Print duplicate-name conflicts beneath the task list so a shadowed
/// task, one the bare-name lookup silently will *not* run (e.g. `cargo
/// run` losing to `just run`), doesn't go unnoticed. Resolution uses the
/// same precedence as `runner run`, so the named winner is what actually
/// executes. No output when there are no conflicts.
pub(super) fn print_conflicts(ctx: &ProjectContext, overrides: &ResolutionOverrides) {
    if let Some(report) = format_conflicts(ctx, overrides, std::io::stdout().is_terminal()) {
        print!("{report}");
    }
}

/// Render the duplicate-name conflict footer, or `None` when there are
/// none. Leading blank line + trailing newline so callers can append it
/// verbatim after the task list.
fn format_conflicts(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    stdout_is_terminal: bool,
) -> Option<String> {
    use std::collections::BTreeMap;

    // Grouped per bare-name scope: the tasks a bare name reaches from here
    // (the root's and the current member's) share one group, every other
    // member is its own group, since those stay reachable as `member:name`.
    let mut by_name: BTreeMap<(&str, &str), Vec<&Task>> = BTreeMap::new();
    for task in &ctx.tasks {
        let scope = if ctx.is_local(task) { "" } else { task.scope() };
        by_name
            .entry((scope, task.name.as_str()))
            .or_default()
            .push(task);
    }

    let conflicts: Vec<(String, &'static str, Vec<&'static str>)> = by_name
        .into_values()
        .filter_map(|group| {
            let group = crate::commands::run::narrow_scope(ctx, group);
            // A single source never duplicates a name; only cross-source
            // collisions are conflicts.
            if group.iter().map(|t| t.source).collect::<HashSet<_>>().len() < 2 {
                return None;
            }
            let winner = crate::commands::run::select_task_entry(ctx, overrides, &group);
            let mut shadowed: Vec<&'static str> = group
                .iter()
                .filter(|t| t.source != winner.source)
                .map(|t| t.source.label())
                .collect();
            shadowed.sort_unstable();
            shadowed.dedup();
            Some((
                ctx.spelling(winner).into_owned(),
                winner.source.label(),
                shadowed,
            ))
        })
        .collect();

    if conflicts.is_empty() {
        return None;
    }

    let count = conflicts.len();
    let header = format!(
        "{count} name conflict{}; `runner run <name>` picks one source:",
        if count == 1 { "" } else { "s" }
    );
    let mut out = String::from("\n");
    let _ = writeln!(
        out,
        "  {}",
        if stdout_is_terminal {
            header.yellow().bold().to_string()
        } else {
            header
        }
    );
    for (name, winner, shadowed) in conflicts {
        let line = format!("{name}: runs {winner}, shadows {}", shadowed.join(", "));
        let _ = writeln!(
            out,
            "    {}",
            if stdout_is_terminal {
                line.dimmed().to_string()
            } else {
                line
            }
        );
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{expected_source_labels, format_conflicts};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{ProjectContext, Task, TaskSource};

    #[test]
    fn invalid_source_error_mentions_pyproject() {
        let ctx = ProjectContext {
            cwd: PathBuf::from("."),
            root: PathBuf::from("."),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        };

        let err = super::list(
            &ctx,
            &ResolutionOverrides::default(),
            false,
            false,
            Some("wat"),
        )
        .expect_err("invalid source should error");

        let message = format!("{err:#}");
        assert!(message.contains("pyproject.toml"));
        assert!(expected_source_labels().contains("pyproject.toml"));
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.into(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        }
    }

    fn ctx_with_tasks(tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("/tmp/conflicts"),
            root: PathBuf::from("/tmp/conflicts"),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn format_conflicts_flags_cross_source_shadowing() {
        let ctx = ctx_with_tasks(vec![
            task("run", TaskSource::Justfile),
            task("run", TaskSource::CargoAliases),
            task("build", TaskSource::Justfile), // single source → not a conflict
        ]);

        let report = format_conflicts(&ctx, &ResolutionOverrides::default(), false)
            .expect("`run` is defined by two sources");

        assert!(report.contains("1 name conflict"), "got: {report}");
        assert!(report.contains("run: runs"), "got: {report}");
        assert!(report.contains("shadows cargo"), "got: {report}");
        assert!(
            !report.contains("build:"),
            "single-source task is not a conflict"
        );
    }

    #[test]
    fn format_conflicts_returns_none_without_collisions() {
        let ctx = ctx_with_tasks(vec![
            task("build", TaskSource::Justfile),
            task("test", TaskSource::CargoAliases),
        ]);
        assert!(format_conflicts(&ctx, &ResolutionOverrides::default(), false).is_none());
    }
}
