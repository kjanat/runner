//! Task-token qualification + pre-flight validation.
//!
//! Pure structural checks on a task string (the qualifier `source:task`
//! syntax, reversed-qualifier detection, runner-constraint matching).
//! No subprocess spawning, no `→` arrow printed, no resolver state
//! consulted — so the chain executor can run [`precheck_task`] on every
//! item *before* any sibling dispatches.

use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::resolver::{ResolutionOverrides, ResolveError};
use crate::types::{ProjectContext, TaskSource};

/// Parse `"source:task"` syntax. Returns `(Some(source), task_name)` if the
/// prefix before the first `:` is a known source label, or `(None, original)`
/// for bare names and names with colons that don't match a source.
pub(super) fn parse_qualified_task(input: &str) -> (Option<TaskSource>, &str) {
    if let Some(colon) = input.find(':') {
        let prefix = &input[..colon];
        if let Some(source) = TaskSource::from_label(prefix) {
            return (Some(source), &input[colon + 1..]);
        }
    }
    (None, input)
}

/// Catch the inverted qualifier syntax (`task:source` instead of the
/// supported `source:task`). Returns `Some((source, task_name))` when
/// the *suffix* after the last `:` names a known source so the caller
/// can surface an actionable error with a `did you mean?` hint instead
/// of falling through to the PM-exec fallback and spawning a binary
/// named after the user's typo.
///
/// Matches only on the last colon so a hypothetical task name with
/// embedded colons (`foo:bar:cargo`) collapses to a single suffix check
/// — if `cargo` is the suffix, suggest `cargo:foo:bar`; otherwise let
/// the existing fallback path handle it.
pub(super) fn detect_reversed_qualifier(input: &str) -> Option<(TaskSource, &str)> {
    let colon = input.rfind(':')?;
    let suffix = &input[colon + 1..];
    let source = TaskSource::from_label(suffix)?;
    Some((source, &input[..colon]))
}

/// Side-effect-free pre-flight check for a single task token.
///
/// Catches errors the resolver would surface *without* any of the
/// expensive or stdio-visible work: no warnings emitted, no `→` arrow
/// printed, no `<pm> --version` probes. Used by chain mode to bail
/// before running *any* sibling task when a later token is clearly
/// broken — otherwise a chain like `bb t lint:cargo` runs `bb` and
/// `t` to completion before surfacing the typo at item 3.
///
/// Returns `Ok(())` on:
/// - matched task (qualified or unqualified),
/// - unmatched task whose dispatch would fall back to bun-test or
///   PM-exec — those paths require resolver state we deliberately
///   skip here; they get their proper error at dispatch time.
///
/// Returns `Err` on:
/// - qualified miss (`justfile:nonexistent`),
/// - reversed qualifier (`lint:cargo`),
/// - runner-constraint mismatch (`--runner just` with no justfile task).
pub(crate) fn precheck_task(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
) -> Result<()> {
    let (qualifier, task_name) = parse_qualified_task(task);
    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task_name).collect();

    let restricted: Vec<_> = if qualifier.is_some() {
        found.clone()
    } else if let Some(allowed) = allowed_runner_sources(overrides) {
        found
            .iter()
            .copied()
            .filter(|t| allowed.contains(&t.source))
            .collect()
    } else {
        found.clone()
    };

    if !restricted.is_empty() {
        // For qualified inputs, also confirm the named source actually
        // produced a candidate; otherwise mirror `resolve_dispatch`'s
        // "not found in <source>" diagnostic.
        if let Some(source) = qualifier
            && !restricted.iter().any(|t| t.source == source)
        {
            bail!("task {task_name:?} not found in {}", source.label());
        }
        return Ok(());
    }

    if let Some(source) = qualifier {
        bail!("task {task_name:?} not found in {}", source.label());
    }

    if let Some((src, task_part)) = detect_reversed_qualifier(task) {
        let src_label = src.label();
        bail!(
            "unknown qualifier in {task:?}: source {src_label:?} must come first.\n\
             hint: did you mean \"{src_label}:{task_part}\"?",
        );
    }

    if let Some(reason) = runner_constraint_error(overrides, &found) {
        return Err(reason.into());
    }

    // Unqualified miss with no constraint and no reversed shape: dispatch
    // will try bun-test / PM-exec fallback. Those require resolver state
    // we intentionally skip here; defer to the dispatch-time path.
    Ok(())
}

/// Compute the set of [`TaskSource`]s the user's runner constraint
/// permits, or `None` when no constraint is active.
///
/// `--runner` / `RUNNER_RUNNER` is the strongest signal — only that
/// runner's source is allowed. `[task_runner].prefer` is the next:
/// every runner in the list is allowed, in listed order. Runners that
/// don't map to a [`TaskSource`] (`nx`, `mise`) are dropped from the
/// permission set; if that leaves the set empty under an active
/// override, [`runner_constraint_error`] surfaces the misconfiguration
/// to the user instead of silently dispatching through the default
/// priority.
pub(crate) fn allowed_runner_sources(
    overrides: &ResolutionOverrides,
) -> Option<HashSet<TaskSource>> {
    if let Some(ovr) = overrides.runner.as_ref() {
        return Some(ovr.runner.task_source().into_iter().collect());
    }
    if !overrides.prefer_runners.is_empty() {
        let set: HashSet<_> = overrides
            .prefer_runners
            .iter()
            .filter_map(|r| r.task_source())
            .collect();
        return Some(set);
    }
    None
}

/// Convert a "no candidate satisfied the runner constraint" outcome
/// into the right [`ResolveError`] for the user.
///
/// Distinguishes three failure shapes the user benefits from seeing
/// separately:
/// - `--runner nx` (a runner with no task-extraction support today) →
///   the override is unsatisfiable in principle, not just here.
/// - `--runner just` but no Justfile in this project → override is
///   set, candidates exist for the task elsewhere, but none under
///   `Justfile`.
/// - `[task_runner].prefer = [...]` with a task only under sources
///   absent from the list → analogous shape for the prefer-list.
pub(crate) fn runner_constraint_error(
    overrides: &ResolutionOverrides,
    found: &[&crate::types::Task],
) -> Option<ResolveError> {
    if let Some(ovr) = overrides.runner.as_ref() {
        let label = ovr.runner.label();
        if ovr.runner.task_source().is_none() {
            return Some(ResolveError::InvalidOverride {
                value: label.to_string(),
                reason: "no task source is registered for this runner; cannot restrict candidates",
            });
        }
        let reason = if found.is_empty() {
            "no task with that name exists in the project"
        } else {
            "no candidate task is registered under this runner's source"
        };
        return Some(ResolveError::InvalidOverride {
            value: label.to_string(),
            reason,
        });
    }
    if !overrides.prefer_runners.is_empty() {
        // Stringify the list once for the user; the static `reason`
        // string can't carry the list, so the dynamic value field
        // does double duty as both the offending input and the
        // detail. Surfaced verbatim by the `Display` impl as
        // `invalid override value "[just, turbo]": ...`.
        let names = overrides
            .prefer_runners
            .iter()
            .map(|r| r.label())
            .collect::<Vec<_>>()
            .join(", ");
        return Some(ResolveError::InvalidOverride {
            value: format!("[{names}]"),
            reason: "[task_runner].prefer matched no candidate task source",
        });
    }
    None
}
