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
use serde::Serialize;

use crate::cmd::run::{
    ResolvedPythonPm, allowed_runner_sources, resolve_python_pm, runner_constraint_error,
    select_task_entry, source_depth, source_priority,
};
use crate::resolver::{ResolutionOverrides, ResolveError, ResolvedPm, Resolver};
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
    schema_version: u32,
) -> Result<()> {
    let candidates: Vec<&Task> = ctx.tasks.iter().filter(|t| t.name == task).collect();
    let restricted: Vec<&Task> = allowed_runner_sources(overrides).map_or_else(
        || candidates.clone(),
        |allowed| {
            candidates
                .iter()
                .copied()
                .filter(|t| allowed.contains(&t.source))
                .collect()
        },
    );

    if restricted.is_empty()
        && let Some(reason) = runner_constraint_error(overrides, &candidates)
    {
        return Err(reason.into());
    }

    let selected = (!restricted.is_empty()).then(|| select_task_entry(ctx, overrides, &restricted));

    let pm_decision = pm_decision_for_selected(ctx, overrides, selected);

    let report = build_report(
        task,
        &candidates,
        selected,
        pm_decision.as_ref(),
        overrides,
        ctx,
        schema_version,
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

enum PmDecision {
    Node(Result<ResolvedPm, ResolveError>),
    Python(Result<ResolvedPythonPm, String>),
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
pub(super) struct WhyReport<'a> {
    #[serde(rename = "$schema", skip_serializing_if = "str::is_empty")]
    #[cfg_attr(
        feature = "schema",
        schemars(description = "URI of the JSON Schema that describes this payload.")
    )]
    schema: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Schema contract version for this JSON payload.")
    )]
    schema_version: u32,
    task: &'a str,
    candidates: Vec<WhyCandidate<'a>>,
    selected: Option<WhyCandidate<'a>>,
    pm_resolution: Option<PmResolution>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
struct WhyCandidate<'a> {
    source: &'static str,
    source_priority: u16,
    depth: Option<usize>,
    display_order: u8,
    is_alias: bool,
    alias_of: Option<&'a str>,
    description: Option<&'a str>,
    passthrough_to: Option<&'static str>,
    source_dir: Option<String>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum PmResolution {
    Resolved {
        pm: &'static str,
        via: String,
        warnings: Vec<WhyWarning>,
    },
    Error {
        error: String,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
struct WhyWarning {
    source: &'static str,
    detail: String,
}

fn pm_decision_for_selected(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    selected: Option<&Task>,
) -> Option<PmDecision> {
    match selected.map(|task| task.source) {
        Some(TaskSource::PackageJson) => {
            Some(PmDecision::Node(Resolver::new(ctx, overrides).resolve_node_pm()))
        }
        Some(TaskSource::PyprojectScripts) => Some(PmDecision::Python(
            resolve_python_pm(ctx, overrides).ok_or_else(|| {
                "no Python package manager detected to run pyproject scripts; install uv, poetry, or pipenv"
                    .to_string()
            }),
        )),
        _ => None,
    }
}

fn build_report<'a>(
    task: &'a str,
    candidates: &[&'a Task],
    selected: Option<&'a Task>,
    pm_decision: Option<&PmDecision>,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
    schema_version: u32,
) -> WhyReport<'a> {
    WhyReport {
        schema: String::new(),
        schema_version,
        task,
        candidates: candidates
            .iter()
            .map(|candidate| candidate_json(candidate, overrides, ctx, schema_version))
            .collect::<Vec<_>>(),
        selected: selected.map(|task| candidate_json(task, overrides, ctx, schema_version)),
        pm_resolution: pm_decision.map(pm_resolution),
    }
}

fn pm_resolution(decision: &PmDecision) -> PmResolution {
    match decision {
        PmDecision::Node(Ok(decision)) => PmResolution::Resolved {
            pm: decision.pm.label(),
            via: decision.describe(),
            warnings: decision
                .warnings
                .iter()
                .map(|warning| WhyWarning {
                    source: warning.source(),
                    detail: warning.detail(),
                })
                .collect(),
        },
        PmDecision::Node(Err(err)) => PmResolution::Error {
            error: format!("{err}"),
        },
        PmDecision::Python(Ok(decision)) => PmResolution::Resolved {
            pm: decision.pm.label(),
            via: decision.describe(),
            warnings: Vec::new(),
        },
        PmDecision::Python(Err(err)) => PmResolution::Error { error: err.clone() },
    }
}

fn candidate_json<'a>(
    task: &'a Task,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
    schema_version: u32,
) -> WhyCandidate<'a> {
    let depth = source_depth(ctx, task.source);
    let depth = if depth == usize::MAX {
        None
    } else {
        Some(depth)
    };
    WhyCandidate {
        source: crate::schema::labels::source_label_for(task.source, schema_version),
        source_priority: source_priority(overrides, task.source),
        depth,
        display_order: task.source.display_order(),
        is_alias: task.alias_of.is_some(),
        alias_of: task.alias_of.as_deref(),
        description: task.description.as_deref(),
        passthrough_to: task.passthrough_to.map(crate::types::TaskRunner::label),
        source_dir: source_dir_for_task(task, ctx).map(|path| path.display().to_string()),
    }
}

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
        TaskSource::GoPackage => tool::go_pm::find_file(&ctx.root),
        TaskSource::BaconToml => tool::files::find_first(&ctx.root, tool::bacon::FILENAMES),
        TaskSource::MiseToml => tool::mise::find_file(&ctx.root),
        TaskSource::PyprojectScripts => tool::python::find_pyproject_upwards(&ctx.root),
    }
}

fn print_human(
    task: &str,
    candidates: &[&Task],
    selected: Option<&Task>,
    pm_decision: Option<&PmDecision>,
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
            PmDecision::Node(Ok(decision)) => {
                println!("  {}", decision.describe());
                for w in &decision.warnings {
                    println!("  {} {w}", "warn:".yellow().bold());
                }
            }
            PmDecision::Node(Err(err)) => {
                println!("  {} {err}", "error:".red().bold());
            }
            PmDecision::Python(Ok(decision)) => println!("  {}", decision.describe()),
            PmDecision::Python(Err(err)) => println!("  {} {err}", "error:".red().bold()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{PmDecision, build_report, pm_decision_for_selected, why};
    use crate::resolver::{DiagnosticFlags, ResolutionOverrides};
    use crate::types::{PackageManager, ProjectContext, Task, TaskSource};

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
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn why_handles_missing_task() {
        let ctx = context(vec![]);
        why(
            &ctx,
            &ResolutionOverrides::default(),
            "build",
            true,
            crate::schema::CURRENT_VERSION,
        )
        .expect("why should succeed even when task is missing");
    }

    #[test]
    fn why_with_multiple_candidates_renders_both_formats() {
        let ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::Justfile),
        ]);
        let version = crate::schema::CURRENT_VERSION;
        why(
            &ctx,
            &ResolutionOverrides::default(),
            "build",
            true,
            version,
        )
        .expect("json should succeed");
        why(
            &ctx,
            &ResolutionOverrides::default(),
            "build",
            false,
            version,
        )
        .expect("human should succeed");
    }

    #[test]
    fn why_rejects_runner_constraint_mismatch() {
        let ctx = context(vec![task("build", TaskSource::PackageJson)]);
        let overrides = ResolutionOverrides::from_cli_and_env(
            None,
            Some("just"),
            None,
            None,
            DiagnosticFlags::default(),
            crate::cli::ChainFailureFlags::default(),
            None,
        )
        .expect("runner override should parse");

        let err = why(
            &ctx,
            &overrides,
            "build",
            true,
            crate::schema::CURRENT_VERSION,
        )
        .expect_err("why should mirror run runner constraints");

        assert!(format!("{err}").contains("no candidate task is registered"));
    }

    #[test]
    fn why_pyproject_script_reports_detected_python_pm() {
        let mut ctx = context(vec![task("greenpy", TaskSource::PyprojectScripts)]);
        ctx.package_managers.push(PackageManager::Uv);
        let selected = ctx.tasks.first();
        let pm_decision = pm_decision_for_selected(&ctx, &ResolutionOverrides::default(), selected)
            .expect("pyproject task should resolve PM diagnostics");

        let report = build_report(
            "greenpy",
            &[&ctx.tasks[0]],
            selected,
            Some(&pm_decision),
            &ResolutionOverrides::default(),
            &ctx,
            crate::schema::CURRENT_VERSION,
        );
        let report = serde_json::to_value(report).expect("why report should serialize");

        assert_eq!(report["pm_resolution"]["pm"], serde_json::json!("uv"));
        assert!(
            report["pm_resolution"]["via"]
                .as_str()
                .is_some_and(|via| via.contains("detected Python project"))
        );
    }

    #[test]
    fn why_pyproject_script_reports_python_pm_override() {
        let ctx = context(vec![task("greenpy", TaskSource::PyprojectScripts)]);
        let overrides = ResolutionOverrides::from_cli_and_env(
            Some("uv"),
            None,
            None,
            None,
            DiagnosticFlags::default(),
            crate::cli::ChainFailureFlags::default(),
            None,
        )
        .expect("PM override should parse");
        let selected = ctx.tasks.first();
        let pm_decision = pm_decision_for_selected(&ctx, &overrides, selected)
            .expect("pyproject task should resolve PM diagnostics");

        match pm_decision {
            PmDecision::Python(Ok(decision)) => {
                assert_eq!(decision.pm, PackageManager::Uv);
                assert!(decision.describe().contains("--pm"));
            }
            PmDecision::Python(Err(err)) => panic!("override should resolve: {err}"),
            PmDecision::Node(_) => panic!("pyproject script should use Python PM resolver"),
        }
    }
}
