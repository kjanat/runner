//! `runner why <task>`, explain how a specific task name would be
//! dispatched.
//!
//! Walks the same source-selection chain used by `runner run`, plus the PM
//! resolution chain when a `package.json` script is in the candidate set,
//! and reports what would happen step by step. Pairs with `runner doctor`
//! (project-wide diagnostic) and `--explain` (one-line trace at run time).

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use crate::cmd::run::{
    ResolvedPythonPm, TokenLookup, allowed_runner_sources, lookup_token, resolve_python_pm,
    runner_constraint_error, select_task_entry, source_depth, source_priority,
};
use crate::resolver::{ResolutionOverrides, ResolveError, ResolvedPm, Resolver};
use crate::schema::labels;
use crate::types::{JsRuntime, ProjectContext, Task, TaskSource};

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
    // Interpret the token exactly like `run` does: qualified syntax
    // (`deno:lint`), FQN (`root:package.json#name`), and the exact-name
    // fallback for colon-named scripts all resolve here, so `why` can
    // explain the very dispatch `run` would perform for the same token.
    let (lookup, candidates) = lookup_token(ctx, task);
    let TokenLookup { qualifier, .. } = lookup;

    // A qualifier pins the source and outranks the runner constraint,
    // mirroring `resolve_dispatch`.
    let restricted: Vec<&Task> = qualifier.map_or_else(
        || {
            allowed_runner_sources(overrides).map_or_else(
                || candidates.clone(),
                |allowed| {
                    candidates
                        .iter()
                        .copied()
                        .filter(|t| allowed.contains(&t.source))
                        .collect()
                },
            )
        },
        |source| {
            candidates
                .iter()
                .copied()
                .filter(|t| t.source == source)
                .collect()
        },
    );

    if restricted.is_empty()
        && qualifier.is_none()
        && let Some(reason) = runner_constraint_error(overrides, &candidates)
    {
        return Err(reason.into());
    }

    let selected = (!restricted.is_empty()).then(|| select_task_entry(ctx, overrides, &restricted));

    let pm_decision = pm_decision_for_selected(ctx, overrides, selected);

    if json {
        let report = build_report(
            task,
            &candidates,
            selected,
            pm_decision.as_ref(),
            overrides,
            ctx,
            qualifier,
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

/// The forced JS runtime and whether it reaches the selected task, so a
/// consumer can reconcile `selected.task.resolved` against the PM block.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyRuntime {
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Forced JS runtime label (`node`, `bun`, or `deno`).",
            extend("enum" = ["node", "bun", "deno"])
        )
    )]
    runtime: &'static str,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Where the runtime override came from (CLI, env, or config).")
    )]
    via: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Whether the selected task dispatches on this runtime. Null when no \
                           task was selected."
        )
    )]
    applied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Why the runtime did not apply, or the lifecycle scripts `node --run` \
                           will skip."
        )
    )]
    note: Option<String>,
}

/// Whether a forced runtime dispatches `source` itself, so `cmd::run` skips
/// package-manager resolution for it.
fn runtime_supersedes_pm(overrides: &ResolutionOverrides, source: TaskSource) -> bool {
    overrides
        .js_runtime()
        .is_some_and(|rt| crate::cmd::run::runtime_honors(source, rt))
}

/// The runtime block: the override plus whether it reaches `selected`.
fn runtime_report(
    overrides: &ResolutionOverrides,
    selected: Option<&Task>,
    ctx: &ProjectContext,
) -> Option<WhyRuntime> {
    let over = overrides.runtime.as_ref()?;
    let runtime = over.runtime;
    let (applied, note) = match selected {
        Some(task) if crate::cmd::run::runtime_honors(task.source, runtime) => {
            let note = (runtime == JsRuntime::Node)
                .then(|| lifecycle_note(ctx, &task.name))
                .flatten();
            (Some(true), note)
        }
        Some(task) => (
            Some(false),
            Some(format!(
                "{name} dispatches through {source}; runtime not applied",
                name = task.name,
                source = task.source.label(),
            )),
        ),
        None => (None, None),
    };
    Some(WhyRuntime {
        runtime: runtime.label(),
        via: over.describe(),
        applied,
        note,
    })
}

/// `node --run` omits `pre`/`post` scripts the project defines; name them.
fn lifecycle_note(ctx: &ProjectContext, task: &str) -> Option<String> {
    let skipped = crate::cmd::run::runtime_lifecycle_scripts(ctx, task);
    (!skipped.is_empty())
        .then(|| format!("node --run skips lifecycle scripts: {}", skipped.join(", ")))
}

fn pm_decision_for_selected(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    selected: Option<&Task>,
) -> Option<PmDecision> {
    match selected.map(|task| task.source) {
        // A forced runtime that dispatches the script reads it through its own
        // runner, so `cmd::run` never resolves a node PM; reporting one here
        // would describe a decision dispatch does not make.
        Some(TaskSource::PackageJson)
            if runtime_supersedes_pm(overrides, TaskSource::PackageJson) =>
        {
            None
        }
        Some(TaskSource::PackageJson) => Some(PmDecision::Node(
            Resolver::new(ctx, overrides).resolve_node_pm(),
        )),
        Some(TaskSource::PyprojectScripts) => Some(PmDecision::Python(
            resolve_python_pm(ctx, overrides).ok_or_else(|| {
                "no Python package manager detected to run pyproject scripts; install uv, poetry, \
                 or pipenv"
                    .to_string()
            }),
        )),
        _ => None,
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

/// `runner why --json` payload. Field order mirrors the committed
/// `schemas/why.example.json`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<WhyRuntime>,
    selected: Option<WhyCandidate<'a>>,
    candidates: Vec<WhyCandidate<'a>>,
    decision: WhyDecision,
}

/// One candidate: the task's identity plus how it matched the query.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyCandidate<'a> {
    task: WhyTask<'a>,
    #[serde(rename = "match")]
    matched: WhyMatch<'a>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyTask<'a> {
    name: &'a str,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Stable task identity: `<scope>:<kind>#<name>`. The `#` boundary keeps \
                           a task name containing `:` (e.g. `fmt:update`) unambiguous. Scope is \
                           `root` until workspace-member scoping lands."
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
        schemars(
            description = "Task mechanism label (structured source label, e.g. `cargo-alias`)."
        )
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
            description = "Locator inside the source file: a key path for structured configs \
                           (`alias.t`, `scripts.test`), the target/recipe name for flat files."
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
            description = "Effective command preview. Null when it depends on a PM resolution \
                           that was not performed for this candidate."
        )
    )]
    resolved: Option<String>,
    cwd: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Task dependencies. Always empty today: no extractor records dependency \
                           edges yet."
        )
    )]
    dependencies: Vec<String>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct WhyMatch<'a> {
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
struct WhyDecision {
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Selection branch taken: `single-candidate`, `ranked`, \
                                `filtered`, or `exec-fallback`.")
    )]
    strategy: &'static str,
    reason: String,
}

fn build_report<'a>(
    query: &'a str,
    candidates: &[&'a Task],
    selected: Option<&'a Task>,
    pm_decision: Option<&PmDecision>,
    overrides: &ResolutionOverrides,
    ctx: &'a ProjectContext,
    qualifier: Option<TaskSource>,
) -> WhyReport<'a> {
    let runtime = overrides.js_runtime();
    let candidate_report = |task: &'a Task| WhyCandidate {
        task: task_report(task, ctx, runtime, pm_decision, selected),
        matched: match_report(query, task, overrides, ctx),
    };
    WhyReport {
        schema: crate::schema::schema_url("why"),
        schema_version: crate::schema::SCHEMA_VERSION,
        kind: "runner.why",
        root: ctx.root.display().to_string(),
        query,
        pm_resolution: pm_decision.map(pm_resolution),
        runtime: runtime_report(overrides, selected, ctx),
        selected: selected.map(candidate_report),
        candidates: candidates.iter().copied().map(candidate_report).collect(),
        decision: decision_report(candidates, selected, qualifier),
    }
}

fn task_report<'a>(
    task: &'a Task,
    ctx: &'a ProjectContext,
    runtime: Option<JsRuntime>,
    pm_decision: Option<&PmDecision>,
    selected: Option<&Task>,
) -> WhyTask<'a> {
    let kind = labels::structured_source_label(task.source);
    let is_selected = selected.is_some_and(|sel| std::ptr::eq(sel, task));
    WhyTask {
        name: &task.name,
        fqn: labels::fqn(task.source, &task.name),
        provider: provider_label(task.source),
        kind,
        source: labels::source_anchor(task.source, &ctx.root)
            .map(|path| path.display().to_string()),
        source_pointer: labels::source_pointer(task),
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
        resolved: resolved_command(task, runtime, pm_decision.filter(|_| is_selected)),
        cwd: ctx.root.display().to_string(),
        dependencies: Vec::new(),
    }
}

fn match_report<'a>(
    selector: &'a str,
    task: &Task,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
) -> WhyMatch<'a> {
    let depth = source_depth(ctx, task.source);
    WhyMatch {
        selector,
        matched_by: "name",
        depth: (depth != usize::MAX).then_some(depth),
        display_order: task.source.display_order(),
        source_priority: source_priority(overrides, task.source),
        is_alias: task.alias_of.is_some(),
        passthrough_to: task.passthrough_to.map(crate::types::TaskRunner::label),
    }
}

fn decision_report(
    candidates: &[&Task],
    selected: Option<&Task>,
    qualifier: Option<TaskSource>,
) -> WhyDecision {
    if candidates.is_empty() {
        return WhyDecision {
            strategy: "exec-fallback",
            reason: "no task matched; `runner run` would route the name through the primary \
                     package manager's exec primitive"
                .to_string(),
        };
    }
    if selected.is_none() {
        // A --runner/[task_runner].prefer restriction that empties the
        // eligible set errors out in `why()` before this function is ever
        // called (`runner_constraint_error`); the only way to reach this
        // branch with a non-empty `candidates` is a qualifier (`deno:x`)
        // that doesn't match any candidate's source.
        let reason = qualifier.map_or_else(
            || {
                "every candidate was filtered out by --runner/RUNNER_RUNNER restrictions"
                    .to_string()
            },
            |source| {
                format!(
                    "candidates exist for this name, but none are registered under the `{}:` \
                     qualifier",
                    source.label()
                )
            },
        );
        return WhyDecision {
            strategy: "filtered",
            reason,
        };
    }
    if candidates.len() == 1 {
        return WhyDecision {
            strategy: "single-candidate",
            reason: "exact task name matched one candidate".to_string(),
        };
    }
    WhyDecision {
        strategy: "ranked",
        reason: format!(
            "{} candidates; lowest (source_priority, source_depth, display_order, alias-last) key \
             wins",
            candidates.len()
        ),
    }
}

/// Tool family that executes tasks from this source. Distinct from the
/// structured `kind` label, which names the extraction mechanism.
pub(super) const fn provider_label(source: TaskSource) -> &'static str {
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

/// Effective command preview for the candidate. `why` only resolves the
/// PM for the selected task; other candidates report null. Delegates the
/// per-source dispatch to [`labels::resolved_command`], shared with
/// `doctor`.
fn resolved_command(
    task: &Task,
    runtime: Option<JsRuntime>,
    pm_decision: Option<&PmDecision>,
) -> Option<String> {
    let node_pm = match pm_decision {
        Some(PmDecision::Node(Ok(decision))) => Some(decision.pm.label()),
        _ => None,
    };
    let python_pm = match pm_decision {
        Some(PmDecision::Python(Ok(decision))) => Some(decision.pm.label()),
        _ => None,
    };
    labels::resolved_command(task, runtime, node_pm, python_pm)
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
            "-".to_string()
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

    if let Some(rt) = runtime_report(overrides, selected, ctx) {
        println!();
        println!("{}", "Runtime".bold());
        println!("  {}", rt.via);
        match rt.applied {
            Some(true) => match &rt.note {
                Some(note) => println!("  {} {note}", "warn:".yellow().bold()),
                None => println!("  {}", "applied to the selected task".dimmed()),
            },
            Some(false) => {
                if let Some(note) = &rt.note {
                    println!("  {} {note}", "not applied:".yellow().bold());
                }
            }
            None => {}
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
            install_dirs: Vec::new(),
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

    #[test]
    fn why_rejects_runner_constraint_mismatch() {
        let ctx = context(vec![task("build", TaskSource::PackageJson)]);
        let overrides = ResolutionOverrides::from_cli_and_env(
            crate::resolver::CliOverrides {
                runner: Some("just"),
                ..crate::resolver::CliOverrides::default()
            },
            DiagnosticFlags::default(),
            crate::cli::ChainFailureFlags::default(),
            None,
        )
        .expect("runner override should parse");

        let err = why(&ctx, &overrides, "build", true)
            .expect_err("why should mirror run runner constraints");

        assert!(format!("{err}").contains("no candidate task is registered"));
    }

    #[test]
    fn why_pyproject_script_reports_python_pm_override() {
        let ctx = context(vec![task("greenpy", TaskSource::PyprojectScripts)]);
        let overrides = ResolutionOverrides::from_cli_and_env(
            crate::resolver::CliOverrides {
                pm: Some("uv"),
                ..crate::resolver::CliOverrides::default()
            },
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

    #[test]
    fn report_describes_cargo_alias_like_the_committed_example() {
        let mut alias = task("t", TaskSource::CargoAliases);
        alias.alias_of = Some("test".to_string());
        let ctx = context(vec![alias]);
        let candidates = vec![&ctx.tasks[0]];
        let selected = ctx.tasks.first();

        let report = build_report(
            "t",
            &candidates,
            selected,
            None,
            &ResolutionOverrides::default(),
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["schema_version"], 2);
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
    fn report_uses_exec_fallback_decision_when_nothing_matches() {
        let ctx = context(vec![]);
        let report = build_report(
            "nope",
            &[],
            None,
            None,
            &ResolutionOverrides::default(),
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["selected"], serde_json::Value::Null);
        assert_eq!(json["candidates"], serde_json::json!([]));
        assert_eq!(json["decision"]["strategy"], "exec-fallback");
    }

    #[test]
    fn report_describes_qualifier_mismatch_as_filtered_not_runner_restricted() {
        // `why deno:build` when "build" exists elsewhere but not under
        // deno.json: `candidates` still lists the same-named tasks
        // (useful diagnostic, `lookup_token` surfaces them precisely so
        // this case is explainable), but nothing is eligible under the
        // `deno:` qualifier, so `selected` is None. The "filtered" reason
        // must name the qualifier, not blame a --runner restriction that
        // was never set.
        let ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::Justfile),
        ]);
        let candidates: Vec<&Task> = ctx.tasks.iter().collect();

        let report = build_report(
            "deno:build",
            &candidates,
            None,
            None,
            &ResolutionOverrides::default(),
            &ctx,
            Some(TaskSource::DenoJson),
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["selected"], serde_json::Value::Null);
        assert_eq!(json["candidates"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["decision"]["strategy"], "filtered");
        assert_eq!(
            json["decision"]["reason"],
            "candidates exist for this name, but none are registered under the `deno:` qualifier"
        );
    }

    #[test]
    fn report_ranks_multiple_candidates() {
        let ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::Justfile),
        ]);
        let candidates: Vec<&Task> = ctx.tasks.iter().collect();
        let report = build_report(
            "build",
            &candidates,
            ctx.tasks.first(),
            None,
            &ResolutionOverrides::default(),
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["decision"]["strategy"], "ranked");
        assert_eq!(json["candidates"].as_array().map(Vec::len), Some(2));
        // package.json resolved depends on PM resolution, which only the
        // selected task gets, and no PM decision was passed here.
        assert_eq!(
            json["candidates"][0]["task"]["resolved"],
            serde_json::Value::Null
        );
        assert_eq!(json["candidates"][1]["task"]["resolved"], "just build");
    }

    #[test]
    fn report_resolves_selected_pyproject_script_through_python_pm() {
        let mut ctx = context(vec![task("greenpy", TaskSource::PyprojectScripts)]);
        ctx.package_managers.push(PackageManager::Uv);
        let selected = ctx.tasks.first();
        let pm_decision = pm_decision_for_selected(&ctx, &ResolutionOverrides::default(), selected)
            .expect("pyproject task should resolve PM diagnostics");
        let candidates = vec![&ctx.tasks[0]];

        let report = build_report(
            "greenpy",
            &candidates,
            selected,
            Some(&pm_decision),
            &ResolutionOverrides::default(),
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["selected"]["task"]["provider"], "python");
        assert_eq!(json["selected"]["task"]["resolved"], "uv run greenpy");
        assert_eq!(
            json["selected"]["task"]["source_pointer"],
            "project.scripts.greenpy"
        );
    }

    fn runtime_overrides(label: &str) -> ResolutionOverrides {
        ResolutionOverrides::from_cli_and_env(
            crate::resolver::CliOverrides {
                runtime: Some(label),
                ..crate::resolver::CliOverrides::default()
            },
            DiagnosticFlags::default(),
            crate::cli::ChainFailureFlags::default(),
            None,
        )
        .expect("runtime override should parse")
    }

    #[test]
    fn forced_runtime_previews_the_runtime_command_not_the_pm_command() {
        let ctx = context(vec![task("build", TaskSource::PackageJson)]);
        let overrides = runtime_overrides("bun");
        let selected = ctx.tasks.first();
        // A forced runtime supersedes PM resolution, exactly as dispatch does.
        let pm_decision = pm_decision_for_selected(&ctx, &overrides, selected);
        assert!(pm_decision.is_none(), "runtime must suppress PM resolution");

        let report = build_report(
            "build",
            &[&ctx.tasks[0]],
            selected,
            None,
            &overrides,
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["selected"]["task"]["resolved"], "bun --bun run build");
        assert_eq!(json["pm_resolution"], serde_json::Value::Null);
        assert_eq!(json["runtime"]["runtime"], "bun");
        assert_eq!(json["runtime"]["applied"], true);
        assert!(
            json["runtime"]["via"]
                .as_str()
                .is_some_and(|v| v.contains("--runtime"))
        );
    }

    #[test]
    fn node_runtime_preview_notes_the_skipped_lifecycle_scripts() {
        let ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("prebuild", TaskSource::PackageJson),
        ]);
        let overrides = runtime_overrides("node");
        let selected = ctx.tasks.first();

        let report = build_report(
            "build",
            &[&ctx.tasks[0]],
            selected,
            None,
            &overrides,
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["selected"]["task"]["resolved"], "node --run build");
        assert_eq!(json["runtime"]["applied"], true);
        assert!(
            json["runtime"]["note"]
                .as_str()
                .is_some_and(|n| n.contains("prebuild")),
            "node runtime must name the lifecycle script it skips: {}",
            json["runtime"]["note"]
        );
    }

    #[test]
    fn forced_runtime_reports_not_applied_for_a_source_it_cannot_honour() {
        let ctx = context(vec![task("build", TaskSource::Justfile)]);
        let overrides = runtime_overrides("bun");
        let selected = ctx.tasks.first();

        let report = build_report(
            "build",
            &[&ctx.tasks[0]],
            selected,
            None,
            &overrides,
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        // The justfile command is untouched; the runtime block says why.
        assert_eq!(json["selected"]["task"]["resolved"], "just build");
        assert_eq!(json["runtime"]["applied"], false);
        assert!(
            json["runtime"]["note"]
                .as_str()
                .is_some_and(|n| n.contains("just") && n.contains("not applied")),
            "note must name the source that won: {}",
            json["runtime"]["note"]
        );
    }

    #[test]
    fn report_collects_sibling_aliases() {
        let mut shortcut = task("f", TaskSource::Justfile);
        shortcut.alias_of = Some("fmt".to_string());
        let ctx = context(vec![task("fmt", TaskSource::Justfile), shortcut]);
        let candidates = vec![&ctx.tasks[0]];

        let report = build_report(
            "fmt",
            &candidates,
            ctx.tasks.first(),
            None,
            &ResolutionOverrides::default(),
            &ctx,
            None,
        );
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(
            json["selected"]["task"]["aliases"],
            serde_json::json!(["f"])
        );
    }
}
