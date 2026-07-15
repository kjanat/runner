//! `runner info`, print detected project context to stdout.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::IsTerminal;

use anyhow::Result;
use colored::Colorize;

use super::list::{print_conflicts, print_tasks_grouped};
use crate::resolver::ResolutionOverrides;
use crate::schema::Project;
use crate::types::{ProjectContext, version_matches};

const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = clap::crate_version!();

/// Display detected package managers, task runners, Node version, monorepo
/// status, and available tasks.
///
/// # Errors
///
/// Returns an error when `--json` is set and `Project` fails to
/// serialize. The human renderer never errors.
pub(crate) fn info(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    json: bool,
) -> Result<()> {
    if json {
        let view = Project::build_with_schema(ctx, overrides, true).into_info_view();
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    super::print_warnings(ctx, overrides, None);

    println!(
        "{}",
        title_line(std::env::args_os().next(), std::io::stdout().is_terminal())
    );
    println!();

    if ctx.package_managers.is_empty() && ctx.task_runners.is_empty() && ctx.tasks.is_empty() {
        println!("  {}", "No project detected in current directory.".dimmed());
        return Ok(());
    }

    if !ctx.package_managers.is_empty() {
        let pms: Vec<&str> = ctx.package_managers.iter().map(|pm| pm.label()).collect();
        println!("  {:<20}{}", "Package Managers".dimmed(), pms.join(", "));
    }

    if !ctx.task_runners.is_empty() {
        let trs: Vec<&str> = ctx.task_runners.iter().map(|tr| tr.label()).collect();
        println!("  {:<20}{}", "Task Runners".dimmed(), trs.join(", "));
    }

    if let Some(nv) = &ctx.node_version {
        let mut line = format!("{} ({})", nv.expected, nv.source);
        if let Some(cur) = &ctx.current_node {
            if version_matches(&nv.expected, cur) {
                let _ = write!(line, ", current {cur} {}", "(ok)".green());
            } else {
                let _ = write!(line, ", current {cur} {}", "(mismatch)".red());
            }
        }
        println!("  {:<20}{}", "Node".dimmed(), line);
    } else if let Some(cur) = &ctx.current_node {
        println!("  {:<20}{}", "Node".dimmed(), cur);
    }

    if ctx.is_monorepo {
        println!("  {:<20}{}", "Monorepo".dimmed(), "yes".green());
    }

    if !ctx.tasks.is_empty() {
        println!();
        // Rows already printed above the task list: title line + the
        // blank after it, each conditional metadata row, and the blank
        // separator just below. The renderer reserves this so the list
        // collapses to compact before the banner pushes it offscreen.
        let banner_rows = 2 // title + trailing blank
            + usize::from(!ctx.package_managers.is_empty())
            + usize::from(!ctx.task_runners.is_empty())
            + usize::from(ctx.node_version.is_some() || ctx.current_node.is_some())
            + usize::from(ctx.is_monorepo)
            + 1; // blank separator before the task list
        let refs: Vec<&crate::types::Task> = ctx.tasks.iter().collect();
        print_tasks_grouped(&refs, &ctx.root, banner_rows);
        print_conflicts(ctx, overrides);
    }
    Ok(())
}

fn title_line(arg0: Option<OsString>, stdout_is_terminal: bool) -> String {
    let label = bin_name_from_arg0(arg0).bold().to_string();
    let version = VERSION.to_string();

    if stdout_is_terminal {
        format!(
            "{} {}",
            osc8_link(&label, REPOSITORY_URL),
            osc8_link(&version, &release_url())
        )
    } else {
        format!("{label} {version}")
    }
}

fn release_url() -> String {
    format!("{REPOSITORY_URL}/releases/tag/v{VERSION}")
}

fn bin_name_from_arg0(arg0: Option<OsString>) -> String {
    // Delegate to the canonical helper so the banner presents the same
    // `run` / `runner` identity as `--version` and `--help`, notably it
    // strips the Windows `.exe` suffix that `argv[0]` carries.
    arg0.and_then(|raw| crate::bin_name_from_arg0(&raw))
        .unwrap_or_else(|| "runner".to_string())
}

fn osc8_link(label: &str, url: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{label}\u{1b}]8;;\u{1b}\\")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{VERSION, bin_name_from_arg0, release_url, title_line};

    #[test]
    fn bin_name_from_arg0_uses_path_file_name() {
        assert_eq!(bin_name_from_arg0(Some(OsString::from("/tmp/run"))), "run");
    }

    #[test]
    fn bin_name_from_arg0_strips_windows_exe_suffix() {
        // The banner must match `--version` / `--help`, which never leak the
        // platform-specific `.exe` extension carried by `argv[0]`. Bare file
        // names are used because `Path::file_name` won't split on `\` when the
        // tests run on Unix.
        assert_eq!(bin_name_from_arg0(Some(OsString::from("run.exe"))), "run");
        assert_eq!(
            bin_name_from_arg0(Some(OsString::from("runner.EXE"))),
            "runner"
        );
    }

    #[test]
    fn bin_name_from_arg0_falls_back_when_absent() {
        assert_eq!(bin_name_from_arg0(None), "runner");
    }

    #[test]
    fn title_line_wraps_label_with_osc8_when_terminal() {
        let line = title_line(Some(OsString::from("run")), true);

        assert!(line.contains("\u{1b}]8;;https://github.com/kjanat/runner\u{1b}\\"));
        assert!(line.contains("run"));
        assert!(line.contains(&format!(
            "\u{1b}]8;;https://github.com/kjanat/runner/releases/tag/v{VERSION}\u{1b}\\{VERSION}\u{1b}]8;;\u{1b}\\"
        )));
    }

    #[test]
    fn title_line_is_plain_when_not_terminal() {
        let line = title_line(Some(OsString::from("run")), false);

        assert!(line.contains("run"));
        assert!(line.contains(VERSION));
        assert!(!line.contains("\u{1b}]8;;"));
    }

    #[test]
    fn release_url_points_to_version_tag() {
        assert_eq!(
            release_url(),
            format!("https://github.com/kjanat/runner/releases/tag/v{VERSION}")
        );
    }
}
