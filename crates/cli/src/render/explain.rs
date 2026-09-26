//! `--dry-run` trace lines.

use colored::Colorize;

use crate::resolver::ResolutionOverrides;

/// Render the command that will execute, without environment values.
pub(crate) fn print_command(overrides: &ResolutionOverrides, command: &std::process::Command) {
    if !overrides.dry_run {
        return;
    }
    print_explain(
        overrides,
        &format!(
            "argv: {:?}",
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .collect::<Vec<_>>()
        ),
    );
    print_explain(
        overrides,
        &format!(
            "cwd: {:?}; env keys: {:?}",
            command.get_current_dir(),
            command.get_envs().map(|(key, _)| key).collect::<Vec<_>>()
        ),
    );
}

pub(crate) fn print_plan(overrides: &ResolutionOverrides, plan: &runner_core::Plan) {
    if !overrides.dry_run {
        return;
    }
    print_explain(
        overrides,
        &format!(
            "trust: {:?}; reach: {:?}; scope: {:?}; evidence: {:?}; decided by: {:?}",
            plan.trust,
            plan.reach,
            plan.scope,
            plan.because.iter().map(|e| &e.at).collect::<Vec<_>>(),
            plan.decided_by
        ),
    );
    if let Some(node) = plan.node {
        print_explain(
            overrides,
            &format!(
                "node: {} answers to node for this command",
                runner_providers::REGISTRY.by_id(node).label
            ),
        );
    }
    for clamp in &plan.clamps {
        print_explain(
            overrides,
            &format!(
                "{} -> {} ({})",
                clamp.requested, clamp.granted, clamp.reason
            ),
        );
    }
}

/// Emit one `--dry-run` trace line (`· runner <body>`), or nothing when
/// dry-run is off. An explicit `--dry-run` overrides quiet presentation so the
/// selected policy and any host limitation remain inspectable.
pub(crate) fn print_explain(overrides: &ResolutionOverrides, body: &str) {
    if !overrides.dry_run {
        return;
    }
    eprintln!("{} {} {body}", "·".dimmed(), "runner".dimmed());
}

pub(crate) fn print_output_explain(overrides: &ResolutionOverrides, task: &str) {
    let (stdout, stderr) = overrides.task_streams_for(task);
    print_explain(
        overrides,
        &format!(
            "output: level={} progress={} warnings={} errors={} groups={} timing={} summary={} \
             fatal_errors={} task.stdout={} task.stderr={}",
            overrides.quiet_level.label(),
            show_hide(overrides.shows_progress_for(task)),
            show_hide(overrides.shows_warnings()),
            show_hide(overrides.shows_errors()),
            show_hide(overrides.emits_groups_for(task)),
            show_hide(overrides.shows_timing_for(task)),
            show_hide(overrides.shows_summary()),
            show_hide(overrides.shows_fatal_errors()),
            stdout.label(),
            stderr.label(),
        ),
    );
}

const fn show_hide(show: bool) -> &'static str {
    if show { "show" } else { "hide" }
}
