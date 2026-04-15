//! `runner info` — print detected project context to stdout.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::Path;

use colored::Colorize;

use super::list::print_tasks_grouped;
use crate::types::{ProjectContext, version_matches};

const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Display detected package managers, task runners, Node version, monorepo
/// status, and available tasks.
pub(crate) fn info(ctx: &ProjectContext) {
    super::print_warnings(ctx);

    println!(
        "{}",
        title_line(std::env::args_os().next(), std::io::stdout().is_terminal())
    );
    println!();

    if ctx.package_managers.is_empty() && ctx.task_runners.is_empty() && ctx.tasks.is_empty() {
        println!("  {}", "No project detected in current directory.".dimmed());
        return;
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
        print_tasks_grouped(ctx);
    }
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
    format!("{REPOSITORY_URL}releases/tag/v{VERSION}")
}

fn bin_name_from_arg0(arg0: Option<OsString>) -> String {
    arg0.and_then(|raw| {
        Path::new(&raw)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
    .filter(|name| !name.is_empty())
    .unwrap_or_else(|| "runner".to_string())
}

fn osc8_link(label: &str, url: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{label}\u{1b}]8;;\u{1b}\\")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{bin_name_from_arg0, release_url, title_line};

    #[test]
    fn bin_name_from_arg0_uses_path_file_name() {
        assert_eq!(bin_name_from_arg0(Some(OsString::from("/tmp/run"))), "run");
    }

    #[test]
    fn title_line_wraps_label_with_osc8_when_terminal() {
        let line = title_line(Some(OsString::from("run")), true);

        assert!(line.contains("\u{1b}]8;;https://github.com/kjanat/runner/\u{1b}\\"));
        assert!(line.contains("run"));
        assert!(line.contains(
            "\u{1b}]8;;https://github.com/kjanat/runner/releases/tag/v0.3.0\u{1b}\\0.3.0\u{1b}]8;;\u{1b}\\"
        ));
    }

    #[test]
    fn title_line_is_plain_when_not_terminal() {
        let line = title_line(Some(OsString::from("run")), false);

        assert!(line.contains("run"));
        assert!(line.contains("0.3.0"));
        assert!(!line.contains("\u{1b}]8;;"));
    }

    #[test]
    fn release_url_points_to_version_tag() {
        assert_eq!(
            release_url(),
            "https://github.com/kjanat/runner/releases/tag/v0.3.0"
        );
    }
}
