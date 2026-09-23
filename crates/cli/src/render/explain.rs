//! `--explain` trace lines.

use colored::Colorize;

use crate::resolver::ResolutionOverrides;

/// Emit one `--explain` trace line (`· runner <body>`), or nothing when
/// explain is off. An explicit `--explain` overrides quiet presentation so the
/// selected policy and any host limitation remain inspectable.
pub(crate) fn print_explain(overrides: &ResolutionOverrides, body: &str) {
    if !overrides.explain {
        return;
    }
    eprintln!("{} {} {body}", "·".dimmed(), "runner".dimmed());
}

pub(crate) fn print_output_explain(overrides: &ResolutionOverrides, task: &str) {
    let (stdout, stderr) = overrides.task_streams_for(task);
    print_explain(
        overrides,
        &format!(
            "output: level={} progress={} warnings={} errors={} groups={} task_timing={} \
             summary={} fatal_errors={} task.stdout={} task.stderr={}",
            overrides.quiet_level.label(),
            show_hide(overrides.shows_progress_for(task)),
            show_hide(overrides.shows_warnings()),
            show_hide(overrides.shows_errors()),
            show_hide(overrides.emits_groups_for(task)),
            show_hide(overrides.shows_task_timing_for(task)),
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
