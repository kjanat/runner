//! The `→` line printed before a dispatch.

use colored::Colorize;

use crate::resolver::ResolutionOverrides;

/// `→ <label> <task_name> [args]`, unless progress is hidden for `task`.
pub(crate) fn print_dispatch_arrow(
    overrides: &ResolutionOverrides,
    task: &str,
    label: &str,
    task_name: &str,
    args: &[String],
) {
    if !overrides.shows_progress_for(task) {
        return;
    }
    eprintln!(
        "{} {} {}{}",
        "→".dimmed(),
        label.dimmed(),
        task_name.bold(),
        if args.is_empty() { "" } else { " [args]" }.dimmed(),
    );
}
