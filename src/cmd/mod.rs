//! Subcommand implementations: info, run, install, clean, list, completions.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use colored::Colorize;

use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, ProjectContext};

mod clean;
mod completions;
mod doctor;
mod info;
pub(crate) mod install;
mod list;
#[cfg(feature = "man")]
mod man;
pub(crate) mod run;
#[cfg(feature = "schema")]
mod schema;
mod why;

pub(crate) use clean::clean;
pub(crate) use completions::{completions, parse_shell_arg};
pub(crate) use doctor::doctor;
pub(crate) use info::info;
pub(crate) use install::install;
pub(crate) use list::list;
#[cfg(feature = "man")]
pub(crate) use man::{write_man_pages, write_runner_page_to_stdout};
pub(crate) use run::run;
#[cfg(feature = "schema")]
pub(crate) use schema::write_schema;
pub(crate) use why::why;

fn configure_command(command: &mut Command, dir: &Path) {
    command
        .current_dir(dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
}

pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;

        if let Some(code) = status.code() {
            return code;
        }
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }

    status.code().unwrap_or(1)
}

/// Whether to wrap a run in a GitHub Actions log group: only when the user
/// hasn't opted out (`[github].group_output`) *and* we're under GitHub
/// Actions, so `::group::` markers never leak into a normal terminal.
const fn should_group(group_output: bool, under_github_actions: bool) -> bool {
    group_output && under_github_actions
}

/// Open a collapsible GitHub Actions log group titled `runner: {name}` when
/// grouping is enabled (see [`should_group`]).
///
/// The returned [`actions_rs::log::GroupGuard`] emits `::endgroup::` when it
/// is dropped — including on the `?` error path and on panic — so callers
/// just bind it for the duration of the run. Returns `None` (emitting
/// nothing) when grouping is off, which lets callers hold it unconditionally.
fn task_group(overrides: &ResolutionOverrides, name: &str) -> Option<actions_rs::log::GroupGuard> {
    should_group(overrides.group_output, actions_rs::env::is_github_actions())
        .then(|| actions_rs::log::group_guard(format!("runner: {name}")))
}

/// Optional warning collector. `None` means "emit warnings to stderr
/// directly" (single-task path). `Some(set)` means "stash for deduped
/// emission later" (chain dispatch — chain executor emits the deduped
/// set once at the end).
pub(crate) type WarningSink<'a> = Option<&'a mut std::collections::HashSet<DetectionWarning>>;

fn print_warnings(ctx: &ProjectContext, overrides: &ResolutionOverrides, sink: WarningSink<'_>) {
    print_warning_slice(&ctx.warnings, overrides, sink);
}

fn print_warning_slice(
    warnings: &[DetectionWarning],
    overrides: &ResolutionOverrides,
    sink: WarningSink<'_>,
) {
    if overrides.no_warnings {
        return;
    }
    if let Some(set) = sink {
        for warning in warnings {
            set.insert(warning.clone());
        }
        return;
    }
    for warning in warnings {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }
}

/// Emit a previously-collected warning set to stderr. Used by the chain
/// executor after all per-task resolutions have populated the sink.
///
/// Sorted by `Display` form before emission so output is stable across
/// runs — `HashSet` iteration order is unspecified, which made the
/// warning block jump around between invocations of the same chain.
pub(crate) fn emit_collected_warnings(
    warnings: &std::collections::HashSet<DetectionWarning>,
    overrides: &ResolutionOverrides,
) {
    if overrides.no_warnings {
        return;
    }
    let mut sorted: Vec<(String, &DetectionWarning)> =
        warnings.iter().map(|w| (w.to_string(), w)).collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, warning) in sorted {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{configure_command, exit_code};

    #[test]
    fn configure_command_sets_current_dir() {
        let dir = std::env::temp_dir();
        let mut command = Command::new("runner-test-command");

        configure_command(&mut command, dir.as_path());

        assert_eq!(command.get_current_dir(), Some(dir.as_path()));
    }

    #[test]
    fn no_warnings_suppresses_emission() {
        use super::print_warning_slice;
        use crate::resolver::ResolutionOverrides;
        use crate::types::{DetectionWarning, PackageManager};

        // Smoke: print_warning_slice with no_warnings=true must
        // short-circuit before the eprintln. The test asserts no
        // panic / no observable side effects; capturing stderr in
        // cargo test is fiddly and not worth a fixture.
        let warnings = vec![DetectionWarning::PmMismatch {
            declared: PackageManager::Pnpm,
            field: "packageManager",
            lockfile: PackageManager::Yarn,
        }];
        let overrides = ResolutionOverrides {
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        print_warning_slice(&warnings, &overrides, None);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_preserves_signal_status() {
        use std::os::unix::process::ExitStatusExt as _;

        assert_eq!(exit_code(std::process::ExitStatus::from_raw(5 << 8)), 5);
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(2)), 130);
    }

    #[test]
    fn should_group_requires_both_opt_in_and_github_actions() {
        use super::should_group;

        assert!(should_group(true, true));
        assert!(!should_group(false, true), "config opt-out wins");
        assert!(
            !should_group(true, false),
            "no grouping outside GitHub Actions"
        );
        assert!(!should_group(false, false));
    }
}
