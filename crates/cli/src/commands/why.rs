//! `runner why <task>`, explain how a specific task name would be
//! dispatched.
//!
//! Walks the same source-selection chain used by `runner run`, plus the PM
//! resolution chain when a `package.json` script is in the candidate set,
//! and reports what would happen step by step. Pairs with `runner doctor`
//! (project-wide diagnostic) and `--dry-run` (one-line trace at run time).

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use runner_core::{Refusal, TaskRank};

use crate::commands::run::core::Prepared;
use crate::commands::run::decision::PmDecision as Decision;
use crate::commands::run::{refusal_error, root_runner};
use crate::provider::Named;
use crate::resolver::ResolutionOverrides;
use crate::schema::labels::{self, RuntimeLabel};
use crate::types::{ProjectContext, Task};
use runner_core::ProviderId;

/// Every task a token addresses, with its rank, lowest first.
type Ranked<'a> = [(&'a Task, TaskRank)];

/// Explain how `task` would resolve in the current project.
///
/// # Errors
///
/// Propagates observation failures and refusals other than a miss.
pub(crate) fn why(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    json: bool,
) -> Result<()> {
    let prepared = crate::commands::run::core::prepare(ctx, overrides, task)
        .map_err(|refusal| refusal_error(ctx, task, &refusal))?;
    let ranked = match crate::commands::run::core::ranked_in(
        ctx,
        &prepared.tree,
        &prepared.project,
        &prepared.policy,
        task,
    ) {
        Ok(ranked) => ranked,
        Err(Refusal::NoSourceTask { .. }) => Vec::new(),
        Err(refusal) => return Err(refusal_error(ctx, task, &refusal)),
    };
    let outcome = prepared.preview(ctx, overrides, task);
    let (selected, refused) = match prepared.selected(ctx, task) {
        Ok(selected) => (selected, None),
        Err(
            refusal @ (Refusal::NotFound { .. }
            | Refusal::Ambiguous { .. }
            | Refusal::NoSourceTask { .. }),
        ) => (None, Some(refusal)),
        Err(refusal) => return Err(refusal_error(ctx, task, &refusal)),
    };
    let ambiguous = match &refused {
        Some(Refusal::Ambiguous { candidates, .. }) => {
            let mut members: Vec<String> = Vec::new();
            for (_, scope) in candidates {
                let label = scope.label().to_owned();
                if !members.contains(&label) {
                    members.push(label);
                }
            }
            Some(members)
        }
        _ => None,
    };
    let filtered = matches!(refused, Some(Refusal::NoSourceTask { .. }));
    let root = ranked
        .is_empty()
        .then(|| root_runner(ctx, overrides, task))
        .flatten();

    let verdict = Verdict {
        ambiguous: ambiguous.as_deref(),
        filtered,
        root,
        reason: crate::commands::run::core::rank_reason(&ranked),
        outcome: &outcome,
    };
    let pm_decision = pm_decision_for_selected(&prepared, overrides, selected);

    if !json && print_cascade_result(&outcome, task, root.is_some(), ambiguous.is_some()) {
        return Ok(());
    }
    if json {
        let decision = decision_report(&ranked, selected, verdict);
        let report = build_report(
            task,
            &ranked,
            selected,
            pm_decision.as_ref(),
            overrides,
            ctx,
            Explanation {
                decision,
                outcome: &outcome,
            },
        );
        crate::render::json::print(&report)?;
    } else if let Some(runner) = root {
        print_root(task, runner);
    } else {
        print_human(
            task,
            &ranked,
            selected,
            pm_decision.as_ref(),
            overrides,
            ctx,
            verdict,
        );
    }

    Ok(())
}

/// What selection concluded besides the ranking itself.
#[derive(Clone, Copy)]
struct Verdict<'a> {
    /// The workspace members that each define the name, when none is nearer.
    ambiguous: Option<&'a [String]>,
    /// Whether the chosen runner defines none of the candidates.
    filtered: bool,
    /// The task runner whose own entry point takes the token.
    root: Option<ProviderId>,
    /// Why the first candidate outranks the second.
    reason: &'static str,
    /// What the cascade would dispatch.
    outcome: &'a Preview,
}

/// The human report for a token `run` hands to a task runner's own entry
/// point.
fn print_root(task: &str, runner: ProviderId) {
    println!("{} {}", "runner why".bold(), task.bold());
    println!();
    println!(
        "  {}",
        format!(
            "No task with that name; `runner run {task}` invokes {}'s own entry point and lets it \
             pick its default target.",
            runner.label()
        )
        .dimmed()
    );
}

/// The package-manager decision for the selected task, or why there is none.
type PmDecision = Result<(Decision, Vec<WhyWarning>), String>;

#[derive(schemars::JsonSchema, Debug, Serialize)]
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

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct WhyWarning {
    source: &'static str,
    detail: String,
}

/// The forced JS runtime and whether it reaches the selected task, so a
/// consumer can reconcile `selected.task.resolved` against the PM block.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyRuntime {
    #[schemars(description = "Forced JS runtime label.")]
    runtime: RuntimeLabel,
    #[schemars(description = "Where the runtime override came from (CLI, env, or config).")]
    via: String,
    #[schemars(
        description = "Whether the selected task dispatches on this runtime. Null when no task \
                       was selected."
    )]
    applied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Why the runtime did not apply, or what the runtime warns about for the \
                       selected task."
    )]
    note: Option<String>,
}

/// Render non-task cascade outcomes, leaving task details to the report below.
type Preview = Result<(runner_core::Rung, runner_core::Dispatch), Refusal>;

fn print_cascade_result(outcome: &Preview, task: &str, root: bool, ambiguous: bool) -> bool {
    match outcome {
        Ok((rung, outcome)) if rung.name != "task" && !root => {
            match outcome {
                runner_core::Dispatch::Builtin(name) => {
                    println!("Built-in {name} takes precedence over project tasks.");
                }
                runner_core::Dispatch::Plan(plan) => println!(
                    "Resolved {task:?} at {}: {}",
                    rung.name,
                    plan.argv
                        .iter()
                        .map(|arg| arg.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            }
            return true;
        }
        Err(Refusal::Ambiguous { .. }) if ambiguous => {}
        Err(Refusal::NotFound { tried, .. }) => {
            println!(
                "No plan for {task:?}; tried {}",
                tried.iter().map(|r| r.name).collect::<Vec<_>>().join(", ")
            );
            return true;
        }
        Err(refusal) => {
            println!("Refused {task:?}: {refusal}");
            return true;
        }
        Ok(_) => {}
    }
    false
}

/// Whether the runtime `task` runs on dispatches its source itself, so
/// `commands::run` skips package-manager resolution for it.
fn runtime_supersedes_pm(overrides: &ResolutionOverrides, task: &Task) -> bool {
    overrides
        .runtime_for(&crate::commands::run::task_output_key(task))
        .is_some_and(|over| crate::commands::run::runtime_honors(task.source, over.runtime))
}

/// The runtime block: the override plus whether it reaches `selected`.
fn runtime_report(
    overrides: &ResolutionOverrides,
    selected: Option<&Task>,
    outcome: &Preview,
) -> Option<WhyRuntime> {
    let over = selected.map_or_else(
        || overrides.runtime.clone(),
        |task| overrides.runtime_for(&crate::commands::run::task_output_key(task)),
    )?;
    let runtime = over.runtime;
    let (applied, note) = match selected {
        Some(task) if crate::commands::run::runtime_honors(task.source, runtime) => {
            let note = match outcome {
                Ok((_, runner_core::Dispatch::Plan(plan))) if !plan.warnings.is_empty() => Some(
                    plan.warnings
                        .iter()
                        .map(|warning| warning.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
                _ => None,
            };
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
        runtime: RuntimeLabel(runtime),
        via: over.describe(),
        applied,
        note,
    })
}

fn pm_decision_for_selected(
    prepared: &Prepared,
    overrides: &ResolutionOverrides,
    selected: Option<&Task>,
) -> Option<PmDecision> {
    let task = selected?;
    let source = task.source;
    if !source.is_managed() || runtime_supersedes_pm(overrides, task) {
        return None;
    }
    let scope = selected
        .and_then(crate::commands::run::core::task)
        .map_or(runner_core::Scope::Root, |task| task.scope);
    Some(prepared.decision_in(source, &scope).map_or_else(
        || Err(crate::provider::no_dispatcher(source)),
        |decision| {
            let warnings = decision
                .warnings(&prepared.project)
                .iter()
                .map(|warning| WhyWarning {
                    source: warning.source(),
                    detail: warning.detail(),
                })
                .collect();
            Ok((decision, warnings))
        },
    ))
}

fn pm_resolution(decision: &PmDecision) -> PmResolution {
    match decision {
        Ok((decision, warnings)) => PmResolution::Resolved {
            pm: decision.pm.label(),
            via: decision.describe(),
            warnings: warnings
                .iter()
                .map(|warning| WhyWarning {
                    source: warning.source,
                    detail: warning.detail.clone(),
                })
                .collect(),
        },
        Err(error) => PmResolution::Error {
            error: error.clone(),
        },
    }
}

/// `runner why --json` payload. Field order mirrors the committed
/// `schemas/why.example.json`.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(
    deny_unknown_fields,
    title = "runner why <task> --json",
    description = "JSON schema for `runner why <task> --json`: candidate `{task, match}` pairs \
                   plus the selection decision.",
    extend("$id" = crate::schema::schema_url("why"))
)]
pub(super) struct WhyReport<'a> {
    #[serde(rename = "$schema", skip_serializing_if = "str::is_empty")]
    #[schemars(description = "URI of the JSON Schema that describes this payload.")]
    schema: String,
    #[schemars(
        description = "Schema contract version for this JSON payload.",
        extend("const" = crate::schema::SCHEMA_VERSION)
    )]
    schema_version: u32,
    #[schemars(description = "Payload discriminator; always \"runner.why\".")]
    kind: &'static str,
    #[schemars(description = "Project root the query ran against.")]
    root: String,
    #[schemars(description = "The task selector as the user typed it.")]
    query: &'a str,
    pm_resolution: Option<PmResolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<WhyRuntime>,
    output: WhyOutput,
    selected: Option<WhyCandidate<'a>>,
    candidates: Vec<WhyCandidate<'a>>,
    decision: WhyDecision,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyOutput {
    #[schemars(extend("enum" = ["off", "quiet", "very-quiet", "silent", "mute"]))]
    level: &'static str,
    #[serde(flatten)]
    runner: crate::tool::RunnerOutputPolicy,
    #[schemars(extend("enum" = ["normal", "quiet", "reduced"]))]
    tool: &'static str,
    #[schemars(extend("enum" = ["inherit", "discard"]))]
    stdout: &'static str,
    #[schemars(extend("enum" = ["inherit", "discard"]))]
    stderr: &'static str,
}

/// One candidate: the task's identity plus how it matched the query.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyCandidate<'a> {
    task: WhyTask<'a>,
    #[serde(rename = "match")]
    matched: WhyMatch<'a>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyTask<'a> {
    name: &'a str,
    #[schemars(
        description = "Stable task identity: `<scope>:<kind>#<name>`. The `#` boundary keeps a \
                       task name containing `:` (e.g. `fmt:update`) unambiguous. Scope is `root` \
                       until workspace-member scoping lands."
    )]
    fqn: String,
    #[schemars(
        description = "Tool family that would execute the task (e.g. `cargo`, `just`, `node`)."
    )]
    provider: labels::FamilyLabel,
    #[schemars(description = "The task source's label, e.g. `cargo`.")]
    kind: labels::SourceLabel,
    #[schemars(description = "Config file the task was extracted from, when resolvable.")]
    source: Option<String>,
    #[schemars(
        description = "Locator inside the source file: a key path for structured configs \
                       (`alias.t`, `scripts.test`), the target/recipe name for flat files."
    )]
    source_pointer: Option<String>,
    description: Option<&'a str>,
    #[schemars(description = "Names of sibling alias tasks that resolve to this task.")]
    aliases: Vec<&'a str>,
    #[schemars(
        description = "Raw definition target: alias expansion or tool-specific run target."
    )]
    definition: Option<&'a str>,
    #[schemars(
        description = "Effective command preview. Null when it depends on a PM resolution that \
                       was not performed for this candidate."
    )]
    resolved: Option<String>,
    cwd: String,
    #[schemars(
        description = "Tasks that run before this one, as the source declares them. Filled from \
                       `mise tasks --json`; empty for sources without dependency edges."
    )]
    dependencies: Vec<String>,
    #[schemars(description = "Tasks the source runs after this one.")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_post: Vec<String>,
    #[schemars(description = "Tasks this one waits for when they are already scheduled.")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    wait_for: Vec<String>,
    #[schemars(description = "`KEY=VALUE` pairs the source sets for the task.")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    env: Vec<String>,
    #[schemars(
        description = "Argument and flag spec in the source's own language (mise: usage KDL)."
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<&'a str>,
    #[schemars(description = "Tool version pins the task declares, as `tool@version`.")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<String>,
    #[schemars(description = "Input globs the source declares for up-to-date checks.")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    sources: Vec<&'a str>,
    #[schemars(description = "Output globs the source declares for up-to-date checks.")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    outputs: Vec<&'a str>,
    #[schemars(description = "Script file backing the task.")]
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<&'a str>,
    #[schemars(description = "Timeout in the source's own duration syntax.")]
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<&'a str>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyMatch<'a> {
    selector: &'a str,
    #[schemars(description = "How the selector matched. `why` matches exact names only today.")]
    matched_by: &'static str,
    #[schemars(description = "The key `runner run` orders candidates by, lowest first.")]
    rank: Option<WhyRank>,
    is_alias: bool,
    passthrough_to: Option<&'static str>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyRank {
    #[schemars(
        description = "0 for sources a chosen package manager or runtime dispatches, 1 for the \
                       rest."
    )]
    tier: usize,
    #[schemars(
        description = "What dispatches the source: the chosen `runtime` or `package-manager`.",
        extend("enum" = ["runtime", "package-manager", null])
    )]
    dispatcher: Option<&'static str>,
    #[schemars(description = "The source's position in its dispatcher's sources.")]
    dispatch_order: Option<usize>,
    #[schemars(description = "The source's own task priority.")]
    priority: u8,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct WhyDecision {
    #[schemars(
        description = "Selection branch taken: `single-candidate`, `ranked`, `filtered`, \
                       `ambiguous`, `runner-root`, `not-found`, `refused`, or the resolving \
                       cascade rung."
    )]
    strategy: &'static str,
    reason: String,
    /// Cascade rungs visited while planning this token.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tried: Vec<&'static str>,
}

struct Explanation<'a> {
    decision: WhyDecision,
    outcome: &'a Preview,
}

fn build_report<'a>(
    query: &'a str,
    ranked: &Ranked<'a>,
    selected: Option<&'a Task>,
    pm_decision: Option<&PmDecision>,
    overrides: &ResolutionOverrides,
    ctx: &'a ProjectContext,
    explanation: Explanation<'_>,
) -> WhyReport<'a> {
    let Explanation {
        mut decision,
        outcome,
    } = explanation;
    let mut selected = selected;
    match outcome {
        Ok((rung, outcome)) => {
            decision.tried = runner_core::CASCADE
                .iter()
                .take_while(|r| r.name != rung.name)
                .map(|r| r.name)
                .chain(std::iter::once(rung.name))
                .collect();
            if matches!(outcome, runner_core::Dispatch::Builtin(_)) {
                selected = None;
                decision.strategy = "builtin";
                decision.reason = "built-in commands take precedence over project tasks".into();
            }
            if selected.is_none() && ranked.is_empty() {
                decision.strategy = rung.name;
            }
        }
        Err(Refusal::NotFound { tried, .. }) => {
            if ranked.is_empty() {
                decision.strategy = "not-found";
                decision.reason = "no cascade rung resolves this token".into();
            }
            decision.tried = tried.iter().map(|r| r.name).collect();
            selected = None;
        }
        Err(Refusal::Ambiguous { .. }) => {
            decision.strategy = "ambiguous";
            selected = None;
        }
        Err(refusal) => {
            decision.strategy = "refused";
            decision.reason = format!("{refusal}");
            selected = None;
        }
    }
    let candidate_report = |task: &'a Task, rank: Option<&TaskRank>| WhyCandidate {
        task: {
            let mut report = task_report(task, ctx);
            if selected.is_some_and(|selected| std::ptr::eq(selected, task))
                && let Ok((_, runner_core::Dispatch::Plan(plan))) = outcome
            {
                report.resolved = Some(labels::planned_command(plan));
                report.cwd = plan.cwd.display().to_string();
            }
            report
        },
        matched: match_report(query, task, rank),
    };
    WhyReport {
        schema: crate::schema::schema_url("why"),
        schema_version: crate::schema::SCHEMA_VERSION,
        kind: "runner.why",
        root: ctx.root.display().to_string(),
        query,
        pm_resolution: pm_decision.map(pm_resolution),
        runtime: runtime_report(overrides, selected, outcome),
        output: output_report(overrides, selected),
        selected: selected.map(|task| candidate_report(task, rank_of(ranked, task))),
        candidates: ranked
            .iter()
            .map(|(task, rank)| candidate_report(task, Some(rank)))
            .collect(),
        decision,
    }
}

fn output_report(overrides: &ResolutionOverrides, selected: Option<&Task>) -> WhyOutput {
    let task_key = selected.map(super::run::task_output_key);
    let output = overrides.output_for(task_key.as_deref());
    WhyOutput {
        level: overrides.quiet_level.label(),
        runner: output.runner,
        tool: output.tool.label(),
        stdout: output.stdout.label(),
        stderr: output.stderr.label(),
    }
}

fn task_report<'a>(task: &'a Task, ctx: &'a ProjectContext) -> WhyTask<'a> {
    WhyTask {
        name: &task.name,
        fqn: labels::fqn(task),
        provider: labels::FamilyLabel(task.source),
        kind: labels::SourceLabel(task.source),
        source: task
            .detail
            .source
            .as_ref()
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
        resolved: None,
        cwd: task.run_dir(&ctx.root).display().to_string(),
        dependencies: task.detail.depends.clone(),
        depends_post: task.detail.depends_post.clone(),
        wait_for: task.detail.wait_for.clone(),
        env: task.detail.env.clone(),
        tools: task
            .detail
            .tools
            .iter()
            .map(|(tool, version)| format!("{tool}@{version}"))
            .collect(),
        usage: task.detail.usage.as_deref(),
        sources: task.detail.sources.iter().map(String::as_str).collect(),
        outputs: task.detail.outputs.iter().map(String::as_str).collect(),
        file: task.detail.file.as_deref(),
        timeout: task.detail.timeout.as_deref(),
    }
}

fn rank_of<'r>(ranked: &'r Ranked<'_>, task: &Task) -> Option<&'r TaskRank> {
    ranked
        .iter()
        .find(|(candidate, _)| std::ptr::eq(*candidate, task))
        .map(|(_, rank)| rank)
}

fn match_report<'a>(selector: &'a str, task: &Task, rank: Option<&TaskRank>) -> WhyMatch<'a> {
    WhyMatch {
        selector,
        matched_by: "name",
        rank: rank.map(|rank| WhyRank {
            tier: rank.tier,
            dispatcher: (rank.dispatch_order != usize::MAX).then_some(if rank.by_package_manager {
                "package-manager"
            } else {
                "runtime"
            }),
            dispatch_order: (rank.dispatch_order != usize::MAX).then_some(rank.dispatch_order),
            priority: rank.priority,
        }),
        is_alias: task.alias_of.is_some(),
        passthrough_to: task.passthrough_to.map(Named::label),
    }
}

fn decision_report(
    ranked: &Ranked<'_>,
    selected: Option<&Task>,
    verdict: Verdict<'_>,
) -> WhyDecision {
    let decision = |strategy, reason: String| WhyDecision {
        tried: Vec::new(),
        strategy,
        reason,
    };
    if let Some(runner) = verdict.root {
        return decision(
            "runner-root",
            format!(
                "no task matched; `runner run` invokes {}'s own entry point and lets it pick its \
                 default target",
                runner.label()
            ),
        );
    }
    if verdict.filtered {
        return decision(
            "filtered",
            "the chosen runner defines no task by this name".to_owned(),
        );
    }
    if ranked.is_empty() {
        return decision(
            "exec-fallback",
            "no task matched; `runner run` would route the name through the primary package \
             manager's exec primitive"
                .to_owned(),
        );
    }
    if let Some(members) = verdict.ambiguous {
        return decision(
            "ambiguous",
            format!(
                "{} workspace members define this name and the root does not; `runner run` \
                 refuses it until qualified as `<member>:<task>` ({})",
                members.len(),
                members.join(", "),
            ),
        );
    }
    let Some(selected) = selected else {
        return decision(
            "filtered",
            "selection chose none of the candidates".to_owned(),
        );
    };
    if ranked.len() == 1 {
        return decision(
            "single-candidate",
            "exact task name matched one candidate".to_owned(),
        );
    }
    decision(
        "ranked",
        format!(
            "{} candidates; {} runs because {}",
            ranked.len(),
            selected.source.label(),
            verdict.reason,
        ),
    )
}

/// The facts the source declared beyond name and description, one line
/// each, only when present.
fn print_detail(task: &Task, ctx: &ProjectContext) {
    let detail = &task.detail;
    let mut lines: Vec<(&str, String)> = Vec::new();
    if let Some(dir) = &detail.dir
        && dir != task.dir(&ctx.root)
    {
        let shown = dir.strip_prefix(&ctx.root).unwrap_or(dir);
        lines.push(("runs in", shown.display().to_string()));
    }
    if !detail.depends.is_empty() {
        lines.push(("depends on", detail.depends.join(", ")));
    }
    if !detail.depends_post.is_empty() {
        lines.push(("followed by", detail.depends_post.join(", ")));
    }
    if !detail.wait_for.is_empty() {
        lines.push(("waits for", detail.wait_for.join(", ")));
    }
    if !detail.env.is_empty() {
        lines.push(("env", detail.env.join(" ")));
    }
    if !detail.tools.is_empty() {
        let tools: Vec<String> = detail
            .tools
            .iter()
            .map(|(tool, version)| format!("{tool}@{version}"))
            .collect();
        lines.push(("tools", tools.join(", ")));
    }
    if !detail.sources.is_empty() {
        lines.push(("sources", detail.sources.join(", ")));
    }
    if !detail.outputs.is_empty() {
        lines.push(("outputs", detail.outputs.join(", ")));
    }
    if let Some(file) = &detail.file {
        lines.push(("file", file.clone()));
    }
    if let Some(timeout) = &detail.timeout {
        lines.push(("timeout", timeout.clone()));
    }
    if let Some(usage) = &detail.usage {
        lines.push(("usage", usage_line(task, ctx, usage)));
    }
    for (label, value) in lines {
        println!("  {:<12}{}", label.dimmed(), value);
    }
}

fn usage_line(task: &Task, ctx: &ProjectContext, usage: &str) -> String {
    // Prefer the signature the source itself renders; fall back to the
    // raw spec when the source cannot be asked for a parsed one.
    let rendered = match crate::commands::run::core::usage(ctx, task) {
        Ok(spec) => spec,
        Err(warning) => {
            eprintln!("warn: {}", warning.message);
            None
        }
    }
    .filter(|spec| !spec.signature.trim().is_empty())
    .map(|spec| format!("{} {}", task.name, spec.signature));
    rendered.unwrap_or_else(|| usage.replace('\n', "\n              "))
}

/// One line per candidate with the rank key that ordered it.
fn print_candidates(ranked: &Ranked<'_>) {
    let shown = |value: usize| {
        if value == usize::MAX {
            "-".to_owned()
        } else {
            value.to_string()
        }
    };
    println!("{}", "Candidates".bold());
    for (c, rank) in ranked {
        let alias_tag = c
            .alias_of
            .as_deref()
            .map_or(String::new(), |target| format!(" → {target}"));
        let passthrough_tag = c.passthrough_to.map_or(String::new(), |r| {
            format!(" (passthrough to {})", r.label())
        });
        let scope_tag = c
            .member
            .as_ref()
            .map_or(String::new(), |member| format!(" ({})", member.name));
        println!(
            "  {} {}{} [tier={}, dispatch={}, priority={}]{}{}",
            "·".dimmed(),
            c.source.label().bold(),
            scope_tag,
            rank.tier,
            shown(rank.dispatch_order),
            rank.priority,
            alias_tag,
            passthrough_tag,
        );
    }
}

fn print_human(
    task: &str,
    ranked: &Ranked<'_>,
    selected: Option<&Task>,
    pm_decision: Option<&PmDecision>,
    overrides: &ResolutionOverrides,
    ctx: &ProjectContext,
    verdict: Verdict<'_>,
) {
    println!("{} {}", "runner why".bold(), task.bold());
    println!();

    if let Some(members) = verdict.ambiguous {
        let spellings: Vec<String> = members
            .iter()
            .map(|member| format!("{member}:{task}"))
            .collect();
        println!(
            "  {}",
            format!(
                "{} workspace members define this name and the root does not; `runner run` \
                 refuses it until qualified: {}",
                members.len(),
                spellings.join(", "),
            )
            .yellow()
        );
        println!();
    }

    if verdict.filtered {
        println!(
            "  {}",
            "The chosen runner defines no task with that name.".dimmed()
        );
        return;
    }

    if ranked.is_empty() {
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

    print_candidates(ranked);
    println!();

    if let Some(sel) = selected {
        println!(
            "{} {} {}",
            "Selected".bold(),
            "→".dimmed(),
            sel.source.label().green()
        );
        println!("  {}", format!("because {}", verdict.reason).dimmed());
        print_detail(sel, ctx);
    }

    if let Some(res) = pm_decision {
        println!();
        println!("{}", "PM resolution".bold());
        match res {
            Ok((decision, warnings)) => {
                println!("  {}", decision.describe());
                for w in warnings {
                    println!("  {} {}: {}", "warn:".yellow().bold(), w.source, w.detail);
                }
            }
            Err(err) => {
                println!("  {} {err}", "error:".red().bold());
            }
        }
    }

    if let Some(rt) = runtime_report(overrides, selected, verdict.outcome) {
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
    let output = output_report(overrides, selected);
    println!();
    println!("{}", "Output policy".bold());
    println!(
        "  level={} {} tool={} stdout={} stderr={}",
        output.level, output.runner, output.tool, output.stdout, output.stderr,
    );
}

#[cfg(test)]
mod tests {

    use super::{PmDecision, Verdict, build_report, decision_report, pm_decision_for_selected};
    use crate::invocation::Origin;
    use crate::resolver::{Invocation, ResolutionOverrides};
    use crate::types::{ProjectContext, Task};
    use runner_core::ProviderId;

    fn context(tasks: Vec<Task>) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        let mut ctx = ProjectContext {
            cwd: root.clone(),
            root,
            tasks,
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        };
        crate::tool::test_support::seed_context(&mut ctx);
        ctx
    }

    fn why(
        ctx: &ProjectContext,
        overrides: &ResolutionOverrides,
        task: &str,
        json: bool,
    ) -> anyhow::Result<()> {
        super::why(ctx, overrides, task, json)
    }

    fn task(name: &str, source: ProviderId) -> Task {
        Task {
            name: name.to_string(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        }
    }

    /// The `why --json` report for `query`, selected like `why` selects it.
    fn report(
        query: &str,
        pm_decision: Option<&PmDecision>,
        overrides: &ResolutionOverrides,
        ctx: &ProjectContext,
    ) -> serde_json::Value {
        let prepared =
            crate::commands::run::core::prepare(ctx, overrides, query).expect("prepared");
        let ranked = crate::commands::run::core::ranked_in(
            ctx,
            &prepared.tree,
            &prepared.project,
            &prepared.policy,
            query,
        )
        .unwrap_or_default();
        let selected = prepared.selected(ctx, query).ok().flatten();
        let outcome = prepared.preview(ctx, overrides, query);
        let verdict = Verdict {
            ambiguous: None,
            filtered: false,
            root: None,
            reason: crate::commands::run::core::rank_reason(&ranked),
            outcome: &outcome,
        };
        let decision = decision_report(&ranked, selected, verdict);
        serde_json::to_value(build_report(
            query,
            &ranked,
            selected,
            pm_decision,
            overrides,
            ctx,
            super::Explanation {
                decision,
                outcome: &outcome,
            },
        ))
        .expect("report should serialize")
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
            task("build", ProviderId::PackageJson),
            task("build", ProviderId::Just),
        ]);
        why(&ctx, &ResolutionOverrides::default(), "build", true).expect("json should succeed");
        why(&ctx, &ResolutionOverrides::default(), "build", false).expect("human should succeed");
    }

    #[test]
    fn why_renders_the_source_choice_refusal() {
        let ctx = context(vec![task("build", ProviderId::PackageJson)]);
        let overrides = ResolutionOverrides::resolve(
            &Invocation {
                source: Some((ProviderId::Just, Origin::Cli)),
                ..Invocation::default()
            },
            None,
        )
        .expect("source resolves");

        why(&ctx, &overrides, "build", true)
            .expect("why stops after plan and renders the refusal like any other outcome");
    }

    #[test]
    fn why_pyproject_script_reports_python_pm_override() {
        let mut ctx = context(vec![task("greenpy", ProviderId::Pyproject)]);
        let overrides = ResolutionOverrides::resolve(
            &Invocation {
                pm: Some((ProviderId::Uv, Origin::Cli)),
                ..Invocation::default()
            },
            None,
        )
        .expect("PM override resolves");
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);
        let selected = ctx.tasks.first();
        let prepared = crate::commands::run::core::prepare(&ctx, &overrides, "greenpy")
            .expect("observation should succeed");
        let pm_decision = pm_decision_for_selected(&prepared, &overrides, selected)
            .expect("pyproject task should resolve PM diagnostics");

        match pm_decision {
            Ok((decision, _)) => {
                assert_eq!(decision.pm, ProviderId::Uv);
                assert!(decision.describe().contains("--pm"));
            }
            Err(err) => panic!("override should resolve: {err}"),
        }
    }

    #[test]
    fn report_describes_cargo_alias_like_the_committed_example() {
        let mut alias = task("t", ProviderId::Cargo);
        alias.alias_of = Some("test".to_string());
        let ctx = context(vec![alias]);

        let json = report("t", None, &ResolutionOverrides::default(), &ctx);

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["kind"], "runner.why");
        assert_eq!(json["query"], "t");
        assert_eq!(json["pm_resolution"], serde_json::Value::Null);

        let task = &json["selected"]["task"];
        assert_eq!(task["name"], "t");
        assert_eq!(task["fqn"], "root:cargo#t");
        assert_eq!(task["provider"], "cargo");
        assert_eq!(task["kind"], "cargo");
        assert_eq!(task["source_pointer"], "alias.t");
        assert_eq!(task["definition"], "test");
        assert_eq!(task["resolved"], "cargo t");
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
    fn report_refuses_an_unmatched_name_and_lists_the_rungs_tried() {
        let ctx = context(vec![]);
        let json = report("nope", None, &ResolutionOverrides::default(), &ctx);

        assert_eq!(json["selected"], serde_json::Value::Null);
        assert_eq!(json["candidates"], serde_json::json!([]));
        assert_eq!(json["decision"]["strategy"], "not-found");
        let tried = json["decision"]["tried"]
            .as_array()
            .expect("every rung tried is listed");
        assert_eq!(tried.last().and_then(|rung| rung.as_str()), Some("exec"));
    }

    #[test]
    fn decision_names_the_runner_root_invocation() {
        let outcome = Err(runner_core::Refusal::Invalid(String::new()));
        let decision = decision_report(
            &[],
            None,
            Verdict {
                ambiguous: None,
                filtered: false,
                root: Some(ProviderId::Make),
                reason: "",
                outcome: &outcome,
            },
        );

        assert_eq!(decision.strategy, "runner-root");
        assert!(decision.reason.contains("make"), "{}", decision.reason);
    }

    #[test]
    fn why_refuses_a_qualified_miss_like_run() {
        let ctx = context(vec![
            task("build", ProviderId::PackageJson),
            task("build", ProviderId::Just),
        ]);

        let err = why(&ctx, &ResolutionOverrides::default(), "deno:build", true)
            .expect_err("deno.json defines no build");
        assert!(format!("{err:#}").contains("not found in deno"), "{err:#}");
    }

    #[test]
    fn report_builtin_takes_precedence_over_a_same_named_task() {
        let ctx = context(vec![task("list", ProviderId::Just)]);
        let json = report("list", None, &ResolutionOverrides::default(), &ctx);
        assert_eq!(json["decision"]["strategy"], "builtin");
        assert_eq!(json["decision"]["tried"], serde_json::json!(["builtin"]));
        assert!(json["selected"].is_null());
    }

    #[test]
    fn report_ranks_multiple_candidates() {
        let mut ctx = context(vec![
            task("build", ProviderId::PackageJson),
            task("build", ProviderId::Just),
        ]);
        crate::tool::test_support::declare(&mut ctx, ProviderId::Npm);
        let json = report("build", None, &ResolutionOverrides::default(), &ctx);

        assert_eq!(json["decision"]["strategy"], "ranked");
        assert_eq!(json["candidates"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["candidates"][0]["task"]["resolved"], "npm run build");
        assert!(json["candidates"][1]["task"]["resolved"].is_null());
    }

    #[test]
    fn report_resolves_selected_pyproject_script_through_python_pm() {
        let mut ctx = context(vec![task("greenpy", ProviderId::Pyproject)]);
        crate::tool::test_support::declare(&mut ctx, ProviderId::Uv);
        crate::tool::test_support::seed_context(&mut ctx);
        let selected = ctx.tasks.first();
        let prepared =
            crate::commands::run::core::prepare(&ctx, &ResolutionOverrides::default(), "greenpy")
                .expect("observation should succeed");
        let pm_decision =
            pm_decision_for_selected(&prepared, &ResolutionOverrides::default(), selected)
                .expect("pyproject task should resolve PM diagnostics");

        let json = report(
            "greenpy",
            Some(&pm_decision),
            &ResolutionOverrides::default(),
            &ctx,
        );

        assert_eq!(json["selected"]["task"]["provider"], "python");
        assert_eq!(json["selected"]["task"]["resolved"], "uv run greenpy");
        assert_eq!(
            json["selected"]["task"]["source_pointer"],
            "project.scripts.greenpy"
        );
    }

    #[test]
    fn the_pm_decision_follows_the_selected_task_into_its_member() {
        use std::sync::Arc;

        let dir = crate::tool::test_support::TempDir::new("why-member-pm");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"root","workspaces":["packages/*"],"scripts":{"root-build":"echo"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n").unwrap();
        let member_dir = dir.path().join("packages").join("app");
        std::fs::create_dir_all(&member_dir).unwrap();
        std::fs::write(
            member_dir.join("package.json"),
            r#"{"name":"app","scripts":{"build":"echo"}}"#,
        )
        .unwrap();
        std::fs::write(member_dir.join("bun.lock"), "").unwrap();
        let member = Arc::new(crate::types::WorkspaceMember::new(
            "app".to_string(),
            "packages/app".to_string(),
            member_dir,
        ));
        let mut ctx = context(vec![
            task("root-build", ProviderId::PackageJson),
            Task {
                member: Some(Arc::clone(&member)),
                ..task("build", ProviderId::PackageJson)
            },
        ]);
        ctx.root = dir.path().to_path_buf();
        ctx.cwd = ctx.root.clone();
        crate::tool::test_support::declare(&mut ctx, ProviderId::Pnpm);
        ctx.workspace = Some(crate::types::Workspace {
            root: ctx.root.clone(),
            kinds: vec!["package.json workspaces"],
            members: vec![member],
            current: None,
        });
        let overrides = ResolutionOverrides::default();
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);
        let prepared = crate::commands::run::core::prepare(&ctx, &overrides, "build")
            .expect("observation should succeed");
        let (decision, _) = pm_decision_for_selected(&prepared, &overrides, ctx.tasks.get(1))
            .expect("a package.json task has a decision")
            .expect("bun is present in the member");
        assert_eq!(decision.pm, ProviderId::Bun);
        let (decision, _) = pm_decision_for_selected(&prepared, &overrides, ctx.tasks.first())
            .expect("a package.json task has a decision")
            .expect("pnpm is present at the root");
        assert_eq!(decision.pm, ProviderId::Pnpm);
    }

    fn runtime_overrides(label: &str) -> ResolutionOverrides {
        ResolutionOverrides::resolve(
            &Invocation {
                runtime: Some((
                    crate::provider::parse_js_runtime(label).expect("a runtime"),
                    Origin::Cli,
                )),
                ..Invocation::default()
            },
            None,
        )
        .expect("runtime override resolves")
    }

    #[test]
    fn forced_runtime_previews_the_runtime_command_not_the_pm_command() {
        let mut ctx = context(vec![task("build", ProviderId::PackageJson)]);
        crate::tool::test_support::declare(&mut ctx, ProviderId::Bun);
        let overrides = runtime_overrides("bun");
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);
        let selected = ctx.tasks.first();
        // A forced runtime supersedes PM resolution, exactly as dispatch does.
        let prepared = crate::commands::run::core::prepare(&ctx, &overrides, "build")
            .expect("observation should succeed");
        let pm_decision = pm_decision_for_selected(&prepared, &overrides, selected);
        assert!(pm_decision.is_none(), "runtime must suppress PM resolution");

        let json = report("build", None, &overrides, &ctx);

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
        let mut ctx = context(vec![
            task("build", ProviderId::PackageJson),
            task("prebuild", ProviderId::PackageJson),
        ]);
        std::fs::write(
            ctx.root.join("package.json"),
            r#"{ "scripts": { "build": "tsc", "prebuild": "gen" } }"#,
        )
        .expect("manifest");
        let overrides = runtime_overrides("node");
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);

        let json = report("build", None, &overrides, &ctx);

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
        let ctx = context(vec![task("build", ProviderId::Just)]);
        let overrides = runtime_overrides("bun");

        let json = report("build", None, &overrides, &ctx);

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
        let mut shortcut = task("f", ProviderId::Just);
        shortcut.alias_of = Some("fmt".to_string());
        let ctx = context(vec![task("fmt", ProviderId::Just), shortcut]);

        let json = report("fmt", None, &ResolutionOverrides::default(), &ctx);

        assert_eq!(
            json["selected"]["task"]["aliases"],
            serde_json::json!(["f"])
        );
    }
}
