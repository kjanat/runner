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

    if json && schema_version >= 3 {
        let report = build_report_v3(
            task,
            &candidates,
            selected,
            pm_decision.as_ref(),
            overrides,
            ctx,
            schema_version,
        );
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if json {
        let report = build_report(
            task,
            &candidates,
            selected,
            pm_decision.as_ref(),
            overrides,
            ctx,
            schema_version,
        );
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

/// `runner why --json --schema-version 3` payload. Field order mirrors
/// the committed `schemas/why.v3.example.json`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub(super) struct WhyReportV3<'a> {
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
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Payload discriminator; always \"runner.why\".")
    )]
    kind: &'static str,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Project root the query ran against.")
    )]
    root: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "The task selector as the user typed it.")
    )]
    query: &'a str,
    pm_resolution: Option<PmResolution>,
    selected: Option<WhyCandidateV3<'a>>,
    candidates: Vec<WhyCandidateV3<'a>>,
    decision: WhyDecisionV3,
}

/// One candidate: the task's identity plus how it matched the query.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyCandidateV3<'a> {
    task: WhyTaskV3<'a>,
    #[serde(rename = "match")]
    matched: WhyMatchV3<'a>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyTaskV3<'a> {
    name: &'a str,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Stable task identity: `<scope>:<kind>#<name>`. The `#` boundary keeps a task name containing `:` (e.g. `fmt:update`) unambiguous. Scope is `root` until workspace-member scoping lands."
        )
    )]
    fqn: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Tool family that would execute the task (e.g. `cargo`, `just`, `node`)."
        )
    )]
    provider: &'static str,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Task mechanism label (v3 source label, e.g. `cargo-alias`).")
    )]
    kind: &'static str,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Config file the task was extracted from, when resolvable.")
    )]
    source: Option<String>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Locator inside the source file: a key path for structured configs (`alias.t`, `scripts.test`), the target/recipe name for flat files."
        )
    )]
    source_pointer: Option<String>,
    description: Option<&'a str>,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Names of sibling alias tasks that resolve to this task.")
    )]
    aliases: Vec<&'a str>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Raw definition target: alias expansion or tool-specific run target."
        )
    )]
    definition: Option<&'a str>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Effective command preview. Null when it depends on a PM resolution that was not performed for this candidate."
        )
    )]
    resolved: Option<String>,
    cwd: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Task dependencies. Always empty today: no extractor records dependency edges yet."
        )
    )]
    dependencies: Vec<String>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyMatchV3<'a> {
    selector: &'a str,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "How the selector matched. `why` matches exact names only today.")
    )]
    matched_by: &'static str,
    depth: Option<usize>,
    display_order: u8,
    source_priority: u16,
    is_alias: bool,
    passthrough_to: Option<&'static str>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyDecisionV3 {
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Selection branch taken: `single-candidate`, `ranked`, `filtered`, or `exec-fallback`."
        )
    )]
    strategy: &'static str,
    reason: String,
}

fn build_report_v3<'a>(
    query: &'a str,
    candidates: &[&'a Task],
    selected: Option<&'a Task>,
    pm_decision: Option<&PmDecision>,
    overrides: &ResolutionOverrides,
    ctx: &'a ProjectContext,
    schema_version: u32,
) -> WhyReportV3<'a> {
    let candidate_v3 = |task: &'a Task| WhyCandidateV3 {
        task: task_v3(task, ctx, pm_decision, selected, schema_version),
        matched: match_v3(query, task, overrides, ctx),
    };
    WhyReportV3 {
        schema: String::new(),
        schema_version,
        kind: "runner.why",
        root: ctx.root.display().to_string(),
        query,
        pm_resolution: pm_decision.map(pm_resolution),
        selected: selected.map(candidate_v3),
        candidates: candidates.iter().copied().map(candidate_v3).collect(),
        decision: decision_v3(candidates, selected),
    }
}

fn task_v3<'a>(
    task: &'a Task,
    ctx: &'a ProjectContext,
    pm_decision: Option<&PmDecision>,
    selected: Option<&Task>,
    schema_version: u32,
) -> WhyTaskV3<'a> {
    let kind = crate::schema::labels::source_label_for(task.source, schema_version);
    let is_selected = selected.is_some_and(|sel| std::ptr::eq(sel, task));
    WhyTaskV3 {
        name: &task.name,
        fqn: crate::schema::labels::fqn(task.source, &task.name, schema_version),
        provider: provider_label(task.source),
        kind,
        source: source_dir_for_task(task, ctx).map(|path| path.display().to_string()),
        source_pointer: source_pointer(task),
        description: task.description.as_deref(),
        aliases: ctx
            .tasks
            .iter()
            .filter(|other| {
                other.source == task.source && other.alias_of.as_deref() == Some(&task.name)
            })
            .map(|other| other.name.as_str())
            .collect(),
        definition: task.alias_of.as_deref().or(task.run_target.as_deref()),
        resolved: resolved_command(task, pm_decision.filter(|_| is_selected)),
        cwd: ctx.root.display().to_string(),
        dependencies: Vec::new(),
    }
}

fn match_v3<'a>(
    selector: &'a str,
    task: &Task,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
) -> WhyMatchV3<'a> {
    let depth = source_depth(ctx, task.source);
    WhyMatchV3 {
        selector,
        matched_by: "name",
        depth: (depth != usize::MAX).then_some(depth),
        display_order: task.source.display_order(),
        source_priority: source_priority(overrides, task.source),
        is_alias: task.alias_of.is_some(),
        passthrough_to: task.passthrough_to.map(crate::types::TaskRunner::label),
    }
}

fn decision_v3(candidates: &[&Task], selected: Option<&Task>) -> WhyDecisionV3 {
    if candidates.is_empty() {
        return WhyDecisionV3 {
            strategy: "exec-fallback",
            reason: "no task matched; `runner run` would route the name through the primary \
                     package manager's exec primitive"
                .to_string(),
        };
    }
    if selected.is_none() {
        return WhyDecisionV3 {
            strategy: "filtered",
            reason: "every candidate was filtered out by --runner/RUNNER_RUNNER restrictions"
                .to_string(),
        };
    }
    if candidates.len() == 1 {
        return WhyDecisionV3 {
            strategy: "single-candidate",
            reason: "exact task name matched one candidate".to_string(),
        };
    }
    WhyDecisionV3 {
        strategy: "ranked",
        reason: format!(
            "{} candidates; lowest (source_priority, source_depth, display_order, alias-last) \
             key wins",
            candidates.len()
        ),
    }
}

/// Tool family that executes tasks from this source. Distinct from the
/// v3 `kind` label, which names the extraction mechanism.
const fn provider_label(source: TaskSource) -> &'static str {
    match source {
        TaskSource::PackageJson => "node",
        TaskSource::DenoJson => "deno",
        TaskSource::TurboJson => "turbo",
        TaskSource::Makefile => "make",
        TaskSource::Justfile => "just",
        TaskSource::Taskfile => "task",
        TaskSource::CargoAliases => "cargo",
        TaskSource::GoPackage => "go",
        TaskSource::BaconToml => "bacon",
        TaskSource::MiseToml => "mise",
        TaskSource::PyprojectScripts => "python",
    }
}

/// Key path (structured configs) or target name (flat files) locating
/// the task inside its source file.
fn source_pointer(task: &Task) -> Option<String> {
    let name = &task.name;
    match task.source {
        TaskSource::CargoAliases => Some(format!("alias.{name}")),
        TaskSource::PackageJson => Some(format!("scripts.{name}")),
        TaskSource::DenoJson
        | TaskSource::TurboJson
        | TaskSource::Taskfile
        | TaskSource::MiseToml => Some(format!("tasks.{name}")),
        TaskSource::BaconToml => Some(format!("jobs.{name}")),
        TaskSource::PyprojectScripts => Some(format!("project.scripts.{name}")),
        TaskSource::Makefile | TaskSource::Justfile => Some(name.clone()),
        TaskSource::GoPackage => None,
    }
}

/// Effective command preview for the candidate. Sources with a fixed
/// executing binary render deterministically; `package.json` and
/// `pyproject.toml` scripts depend on PM resolution, which `why` only
/// performs for the selected task — other candidates report null.
fn resolved_command(task: &Task, pm_decision: Option<&PmDecision>) -> Option<String> {
    let name = &task.name;
    match task.source {
        TaskSource::CargoAliases => Some(task.alias_of.as_deref().map_or_else(
            || format!("cargo {name}"),
            |expansion| format!("cargo {expansion}"),
        )),
        TaskSource::DenoJson => Some(format!("deno task {name}")),
        TaskSource::TurboJson => Some(format!("turbo run {name}")),
        TaskSource::Makefile => Some(format!("make {name}")),
        TaskSource::Justfile => Some(format!("just {name}")),
        TaskSource::Taskfile => Some(format!("task {name}")),
        TaskSource::BaconToml => Some(format!("bacon {name}")),
        TaskSource::MiseToml => Some(format!("mise run {name}")),
        TaskSource::GoPackage => Some(format!(
            "go run {target}",
            target = task.run_target.as_deref().unwrap_or(name)
        )),
        TaskSource::PackageJson => match pm_decision {
            Some(PmDecision::Node(Ok(decision))) => {
                Some(format!("{pm} run {name}", pm = decision.pm.label()))
            }
            _ => None,
        },
        TaskSource::PyprojectScripts => match pm_decision {
            Some(PmDecision::Python(Ok(decision))) => {
                Some(format!("{pm} run {name}", pm = decision.pm.label()))
            }
            _ => None,
        },
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

    use super::{PmDecision, build_report, build_report_v3, pm_decision_for_selected, why};
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
    fn v3_report_describes_cargo_alias_like_the_committed_example() {
        let mut alias = task("t", TaskSource::CargoAliases);
        alias.alias_of = Some("test".to_string());
        let ctx = context(vec![alias]);
        let candidates = vec![&ctx.tasks[0]];
        let selected = ctx.tasks.first();

        let report = build_report_v3(
            "t",
            &candidates,
            selected,
            None,
            &ResolutionOverrides::default(),
            &ctx,
            3,
        );
        let json = serde_json::to_value(&report).expect("v3 report should serialize");

        assert_eq!(json["schema_version"], 3);
        assert_eq!(json["kind"], "runner.why");
        assert_eq!(json["query"], "t");
        assert_eq!(json["pm_resolution"], serde_json::Value::Null);

        let task = &json["selected"]["task"];
        assert_eq!(task["name"], "t");
        assert_eq!(task["fqn"], "root:cargo-alias#t");
        assert_eq!(task["provider"], "cargo");
        assert_eq!(task["kind"], "cargo-alias");
        assert_eq!(task["source_pointer"], "alias.t");
        assert_eq!(task["definition"], "test");
        assert_eq!(task["resolved"], "cargo test");
        assert_eq!(task["dependencies"], serde_json::json!([]));

        let matched = &json["selected"]["match"];
        assert_eq!(matched["selector"], "t");
        assert_eq!(matched["matched_by"], "name");
        assert_eq!(matched["is_alias"], true);

        assert_eq!(json["candidates"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["decision"]["strategy"], "single-candidate");
        assert_eq!(
            json["decision"]["reason"],
            "exact task name matched one candidate"
        );
    }

    #[test]
    fn v3_report_uses_exec_fallback_decision_when_nothing_matches() {
        let ctx = context(vec![]);
        let report = build_report_v3(
            "nope",
            &[],
            None,
            None,
            &ResolutionOverrides::default(),
            &ctx,
            3,
        );
        let json = serde_json::to_value(&report).expect("v3 report should serialize");

        assert_eq!(json["selected"], serde_json::Value::Null);
        assert_eq!(json["candidates"], serde_json::json!([]));
        assert_eq!(json["decision"]["strategy"], "exec-fallback");
    }

    #[test]
    fn v3_report_ranks_multiple_candidates() {
        let ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::Justfile),
        ]);
        let candidates: Vec<&Task> = ctx.tasks.iter().collect();
        let report = build_report_v3(
            "build",
            &candidates,
            ctx.tasks.first(),
            None,
            &ResolutionOverrides::default(),
            &ctx,
            3,
        );
        let json = serde_json::to_value(&report).expect("v3 report should serialize");

        assert_eq!(json["decision"]["strategy"], "ranked");
        assert_eq!(json["candidates"].as_array().map(Vec::len), Some(2));
        // package.json resolved depends on PM resolution, which only the
        // selected task gets — and no PM decision was passed here.
        assert_eq!(
            json["candidates"][0]["task"]["resolved"],
            serde_json::Value::Null
        );
        assert_eq!(json["candidates"][1]["task"]["resolved"], "just build");
    }

    #[test]
    fn v3_report_resolves_selected_pyproject_script_through_python_pm() {
        let mut ctx = context(vec![task("greenpy", TaskSource::PyprojectScripts)]);
        ctx.package_managers.push(PackageManager::Uv);
        let selected = ctx.tasks.first();
        let pm_decision = pm_decision_for_selected(&ctx, &ResolutionOverrides::default(), selected)
            .expect("pyproject task should resolve PM diagnostics");
        let candidates = vec![&ctx.tasks[0]];

        let report = build_report_v3(
            "greenpy",
            &candidates,
            selected,
            Some(&pm_decision),
            &ResolutionOverrides::default(),
            &ctx,
            3,
        );
        let json = serde_json::to_value(&report).expect("v3 report should serialize");

        assert_eq!(json["selected"]["task"]["provider"], "python");
        assert_eq!(json["selected"]["task"]["resolved"], "uv run greenpy");
        assert_eq!(
            json["selected"]["task"]["source_pointer"],
            "project.scripts.greenpy"
        );
    }

    #[test]
    fn v3_report_collects_sibling_aliases() {
        let mut shortcut = task("f", TaskSource::Justfile);
        shortcut.alias_of = Some("fmt".to_string());
        let ctx = context(vec![task("fmt", TaskSource::Justfile), shortcut]);
        let candidates = vec![&ctx.tasks[0]];

        let report = build_report_v3(
            "fmt",
            &candidates,
            ctx.tasks.first(),
            None,
            &ResolutionOverrides::default(),
            &ctx,
            3,
        );
        let json = serde_json::to_value(&report).expect("v3 report should serialize");

        assert_eq!(
            json["selected"]["task"]["aliases"],
            serde_json::json!(["f"])
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
