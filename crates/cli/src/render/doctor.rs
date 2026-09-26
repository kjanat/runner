//! The human `runner doctor` report.

use std::fmt::Write as _;

use colored::Colorize;
use serde_json::{Map, Value};

use crate::commands::install::InstallPlan;
use crate::provider::Named;
use crate::resolver::{ResolutionOverrides, ResolveError};
use runner_core::ProviderId;

/// Everything the human report reads.
#[derive(Clone, Copy)]
pub(crate) struct Human<'a> {
    /// The serialized [`crate::schema::Project`].
    pub report: &'a Value,
    /// The overrides in effect.
    pub overrides: &'a ResolutionOverrides,
    /// The install plan, or why none could be made.
    pub plan: Result<&'a InstallPlan, &'a ResolveError>,
    /// The tool manager the toolchain step runs.
    pub tools: Option<ProviderId>,
    /// mise's own verdict on the project.
    pub health: &'a [crate::schema::doctor::Diagnostic],
}

pub(crate) fn print_human(human: &Human<'_>) {
    let root = human.report["root"].as_str().unwrap_or("?");
    println!(
        "{} {}",
        "runner doctor".bold(),
        format!("@ {root}").dimmed()
    );
    println!();

    print_detected(human.report);
    print_overrides(human);
    print_signals(human.report);
    print_decisions(human);
    print_warnings(human);
}

fn print_detected(report: &Value) {
    let detected = &report["detected"];
    print_section("Detected", |out| {
        let pms = detected["package_managers"].as_array().map_or_default(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        });
        if !pms.is_empty() {
            writeln_field(out, "package managers", &pms);
        }
        let trs = detected["task_runners"].as_array().map_or_default(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        });
        if !trs.is_empty() {
            writeln_field(out, "task runners", &trs);
        }
        for runtime in detected["runtimes"].as_array().into_iter().flatten() {
            let name = runtime["name"].as_str().unwrap_or("?");
            let expected = &runtime["expected"];
            if let Some(version) = expected["version"].as_str() {
                let source = expected["source"].as_str().unwrap_or("?");
                writeln_field(
                    out,
                    &format!("{name} version"),
                    &format!("{version} ({source})"),
                );
            }
        }
        if detected["monorepo"].as_bool() == Some(true) {
            writeln_field(out, "monorepo", "yes");
        }
    });
}

fn print_overrides(human: &Human<'_>) {
    let Human {
        report, overrides, ..
    } = *human;
    print_section("Overrides", |out| {
        for setting in ["pm", "source", "runtime"] {
            if let Some(choice) = report["overrides"][setting].as_object() {
                writeln_field(
                    out,
                    setting,
                    &format!(
                        "{} ({})",
                        choice["value"].as_str().unwrap_or("?"),
                        choice["origin"].as_str().unwrap_or("?")
                    ),
                );
            }
        }
        if overrides.dry_run {
            writeln_field(out, "dry-run", "on");
        }
    });
}

fn print_signals(report: &Value) {
    let empty = Map::new();
    for (source, signals) in report["signals"].as_object().unwrap_or(&empty) {
        print_section(&format!("Signals ({source})"), |out| {
            if let Some(lp) = signals["lockfile_pm"].as_str() {
                writeln_field(out, "lockfile pm", lp);
            }
            if let Some(mp) = signals["manifest_pm"].as_object() {
                let pm = mp.get("pm").and_then(Value::as_str).unwrap_or("?");
                let field = mp.get("source").and_then(Value::as_str).unwrap_or("?");
                let version = mp
                    .get("version")
                    .and_then(Value::as_str)
                    .map_or(String::new(), |v| format!(" {v}"));
                let on_fail = mp.get("on_fail").and_then(Value::as_str).unwrap_or("?");
                writeln_field(
                    out,
                    "manifest pm",
                    &format!("{pm}{version} via {field} (onFail={on_fail})"),
                );
            }
            if let Some(probe) = signals["path_probe"].as_object() {
                let shims = signals["shims"].as_object();
                let _ = writeln!(out, "  {}", "PATH probe".dimmed());
                for (bin, path) in probe {
                    for line in probe_lines(bin, path.as_str(), shims.and_then(|s| s.get(bin))) {
                        let _ = writeln!(out, "{line}");
                    }
                }
            }
        });
    }
}

fn print_decisions(human: &Human<'_>) {
    let Human {
        report,
        plan,
        tools,
        ..
    } = *human;
    print_section("Decisions", |out| {
        let empty = Map::new();
        for (source, decision) in report["decisions"].as_object().unwrap_or(&empty) {
            let label = format!("{source} tasks");
            if let Some(via) = decision.get("via").and_then(Value::as_str) {
                writeln_field(out, &label, via);
            } else {
                let error = decision.get("error").and_then(Value::as_str).unwrap_or("?");
                writeln!(out, "  {:<20}{}", label.red(), error.red())
                    .expect("writeln to String should not fail");
            }
        }
        if let Some(runner) = tools {
            writeln_field(out, "tools", runner.label());
        }
        match plan {
            Ok(plan) => write_install_plan(out, plan),
            Err(err) => {
                writeln!(out, "  {:<20}{}", "install".red(), err.to_string().red())
                    .expect("writeln to String should not fail");
            }
        }
    });
}

fn write_install_plan(out: &mut String, plan: &InstallPlan) {
    let pms = plan
        .pms
        .iter()
        .map(|pm| pm.label())
        .collect::<Vec<_>>()
        .join(", ");
    if !pms.is_empty() {
        writeln_field(out, "install", &pms);
    }
    for shadow in &plan.shadowed {
        writeln_field(
            out,
            shadow.dir,
            &format!(
                "{} installs it, {} shadowed",
                shadow.winner.label(),
                shadow.loser.label(),
            ),
        );
    }
}

fn print_warnings(human: &Human<'_>) {
    let Human { report, health, .. } = *human;
    let mut warnings: Vec<(String, String)> = report["warnings"].as_array().map_or_default(|ws| {
        ws.iter()
            .map(|w| {
                (
                    w["source"].as_str().unwrap_or("?").to_string(),
                    w["detail"].as_str().unwrap_or("?").to_string(),
                )
            })
            .collect()
    });
    warnings.extend(health.iter().map(|issue| {
        (
            issue.source.unwrap_or("health").to_string(),
            issue.message.clone(),
        )
    }));
    if !warnings.is_empty() {
        println!("{}", "Warnings".bold());
        for (source, detail) in &warnings {
            println!("  {} {source}: {detail}", "warn:".yellow().bold());
        }
    }
}

/// The `PATH probe` lines for one manager: its path, or `not found`, on the
/// first line, and where a shim resolves on a second.
fn probe_lines(bin: &str, path: Option<&str>, shim: Option<&Value>) -> Vec<String> {
    let Some(path) = path else {
        return vec![format!("    {bin:<18}{}", "not found".dimmed())];
    };
    let first = format!("    {bin:<18}{path}");
    let Some(shim) = shim else {
        return vec![first];
    };
    let manager = shim["manager"].as_str().unwrap_or("?");
    match shim["resolved"].as_str() {
        Some(real) => vec![
            first,
            format!("{:22}-> {real} {}", "", format!("({manager})").dimmed()),
        ],
        None => vec![format!(
            "{first} {}",
            format!("({manager} shim, not provisioned)").dimmed()
        )],
    }
}

fn print_section<F>(title: &str, fill: F)
where
    F: FnOnce(&mut String),
{
    let mut body = String::new();
    fill(&mut body);
    if body.is_empty() {
        return;
    }
    println!("{}", title.bold());
    print!("{body}");
    println!();
}

fn writeln_field(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "  {:<20}{}", label.dimmed(), value);
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_lines_render_all_four_cases() {
        use serde_json::json;

        use super::probe_lines;

        let not_found = probe_lines("npm", None, None);
        assert_eq!(not_found.len(), 1);
        assert!(not_found[0].starts_with("    npm"), "{not_found:?}");
        assert!(not_found[0].contains("not found"), "{not_found:?}");

        let plain = probe_lines("bun", Some(r"C:\bun\bun.EXE"), None);
        assert_eq!(plain, [format!("    {:<18}{}", "bun", r"C:\bun\bun.EXE")]);

        let shim = json!({ "manager": "volta", "resolved": r"C:\Volta\image\npm\11.6.2\npm.cmd" });
        let resolved = probe_lines("npm", Some(r"C:\Volta\npm.EXE"), Some(&shim));
        assert_eq!(resolved.len(), 2, "{resolved:?}");
        assert!(resolved[0].ends_with(r"C:\Volta\npm.EXE"), "{resolved:?}");
        assert!(
            resolved[1].contains(r"-> C:\Volta\image\npm\11.6.2\npm.cmd"),
            "{resolved:?}"
        );
        assert!(resolved[1].contains("(volta)"), "{resolved:?}");

        let phantom = json!({ "manager": "volta", "resolved": null });
        let unprovisioned = probe_lines("pnpm", Some(r"C:\Volta\pnpm.EXE"), Some(&phantom));
        assert_eq!(unprovisioned.len(), 1);
        assert!(
            unprovisioned[0].contains("volta shim, not provisioned"),
            "{unprovisioned:?}"
        );
    }
}
