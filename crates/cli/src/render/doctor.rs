//! The human `runner doctor` report.

use std::fmt::Write as _;

use colored::Colorize;
use serde_json::{Map, Value};

use crate::commands::install::InstallPlan;
use crate::resolver::{ResolutionOverrides, ResolveError};
use crate::types::TaskRunner;

/// Everything the human report reads.
#[derive(Clone, Copy)]
pub(crate) struct Human<'a> {
    /// The serialized [`crate::schema::Project`].
    pub report: &'a Value,
    /// The overrides in effect.
    pub overrides: &'a ResolutionOverrides,
    /// The install plan, or why none could be made.
    pub plan: Result<&'a InstallPlan, &'a ResolveError>,
    /// Whether the Node sections are shown.
    pub node_context: bool,
    /// The tool manager the toolchain step runs.
    pub tools: Option<TaskRunner>,
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
    print_node_signals(human);
    print_decisions(human);
    print_warnings(human);
}

fn print_detected(report: &Value) {
    let detected = &report["detected"];
    print_section("Detected", |out| {
        let pms = detected["package_managers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if !pms.is_empty() {
            writeln_field(out, "package managers", &pms);
        }
        let trs = detected["task_runners"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if !trs.is_empty() {
            writeln_field(out, "task runners", &trs);
        }
        if let Some(nv) = detected["node_version"].as_object() {
            let expected = nv["expected"].as_str().unwrap_or("?");
            let source = nv["source"].as_str().unwrap_or("?");
            writeln_field(out, "node version", &format!("{expected} ({source})"));
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
        if let Some(pm) = report["overrides"]["pm"].as_object() {
            writeln_field(
                out,
                "pm",
                &format!(
                    "{} ({})",
                    pm["pm"].as_str().unwrap_or("?"),
                    pm["origin"].as_str().unwrap_or("?")
                ),
            );
        }
        let empty = Map::new();
        for (eco, pm) in report["overrides"]["pm_by_ecosystem"]
            .as_object()
            .unwrap_or(&empty)
        {
            writeln_field(
                out,
                &format!("pm.{eco}"),
                &format!(
                    "{} ({})",
                    pm["pm"].as_str().unwrap_or("?"),
                    pm["origin"].as_str().unwrap_or("?")
                ),
            );
        }
        if let Some(r) = report["overrides"]["runner"].as_object() {
            writeln_field(
                out,
                "runner",
                &format!(
                    "{} ({})",
                    r["runner"].as_str().unwrap_or("?"),
                    r["origin"].as_str().unwrap_or("?")
                ),
            );
        }
        writeln_field(
            out,
            "fallback",
            report["overrides"]["fallback"].as_str().unwrap_or("?"),
        );
        if overrides.explain {
            writeln_field(out, "explain", "on");
        }
    });
}

fn print_node_signals(human: &Human<'_>) {
    let Human {
        report,
        node_context,
        ..
    } = *human;
    print_section("Signals (Node)", |out| {
        if !node_context {
            return;
        }
        let node = &report["signals"]["node"];
        if let Some(lp) = node["lockfile_pm"].as_str() {
            writeln_field(out, "lockfile pm", lp);
        }
        if let Some(mp) = node["manifest_pm"].as_object() {
            let pm = mp["pm"].as_str().unwrap_or("?");
            let source = mp["source"].as_str().unwrap_or("?");
            let version = mp["version"]
                .as_str()
                .map_or(String::new(), |v| format!(" {v}"));
            let on_fail = mp["on_fail"].as_str().unwrap_or("?");
            writeln_field(
                out,
                "manifest pm",
                &format!("{pm}{version} via {source} (onFail={on_fail})"),
            );
        }
        if let Some(probe) = node["path_probe"].as_object() {
            let shims = node["volta_shims"].as_object();
            let _ = writeln!(out, "  {}", "PATH probe".dimmed());
            for (bin, path) in probe {
                for line in probe_lines(bin, path.as_str(), shims.and_then(|s| s.get(bin))) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    });
}

fn print_decisions(human: &Human<'_>) {
    let Human {
        report,
        plan,
        node_context,
        tools,
        ..
    } = *human;
    print_section("Decisions", |out| {
        // `Map<String, Value>` indexes panic on missing keys (unlike
        // `Value` indexing, which yields `Null`). Use `.get` so a
        // `node_pm` decision missing its `via` field renders `?`
        // instead of crashing the renderer.
        if let Some(pm) = report["decisions"]["node_pm"]
            .as_object()
            .filter(|_| node_context)
        {
            let via = pm.get("via").and_then(Value::as_str).unwrap_or("?");
            writeln_field(out, "node scripts", via);
        }
        if let Some(err) = report["decisions"]["node_pm_error"]
            .as_str()
            .filter(|_| node_context)
        {
            writeln!(out, "  {:<20}{}", "node scripts".red(), err.red())
                .expect("writeln to String should not fail");
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
    for collision in &plan.collisions {
        let names = collision
            .writers
            .iter()
            .map(|pm| pm.label())
            .collect::<Vec<_>>()
            .join(" then ");
        writeln_field(out, "shared tree", &format!("{names} (serialized)"));
    }
}

fn print_warnings(human: &Human<'_>) {
    let Human {
        report,
        plan,
        health,
        ..
    } = *human;
    // Detection warnings, plus the collisions the install plan kept. The
    // collision is the plan's verdict on the effective install set, not a fact
    // about the tree, so it lives here and nowhere else; commands that never
    // install have nothing to say about it.
    let mut warnings: Vec<(String, String)> = report["warnings"]
        .as_array()
        .map(|ws| {
            ws.iter()
                .map(|w| {
                    (
                        w["source"].as_str().unwrap_or("?").to_string(),
                        w["detail"].as_str().unwrap_or("?").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if let Ok(plan) = plan {
        warnings.extend(plan.collisions.iter().map(|collision| {
            (
                "install".to_string(),
                crate::commands::install::collision_warning(collision.dir, &collision.writers),
            )
        }));
    }
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
/// first line, and where a Volta shim resolves on a second.
fn probe_lines(bin: &str, path: Option<&str>, shim: Option<&Value>) -> Vec<String> {
    let Some(path) = path else {
        return vec![format!("    {bin:<18}{}", "not found".dimmed())];
    };
    let first = format!("    {bin:<18}{path}");
    match shim.map(|s| s["resolved"].as_str()) {
        Some(Some(real)) => vec![first, format!("{:22}-> {real} {}", "", "(volta)".dimmed())],
        Some(None) => vec![format!(
            "{first} {}",
            "(volta shim, not provisioned)".dimmed()
        )],
        None => vec![first],
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

        let shim = json!({ "resolved": r"C:\Volta\image\npm\11.6.2\npm.cmd" });
        let resolved = probe_lines("npm", Some(r"C:\Volta\npm.EXE"), Some(&shim));
        assert_eq!(resolved.len(), 2, "{resolved:?}");
        assert!(resolved[0].ends_with(r"C:\Volta\npm.EXE"), "{resolved:?}");
        assert!(
            resolved[1].contains(r"-> C:\Volta\image\npm\11.6.2\npm.cmd"),
            "{resolved:?}"
        );
        assert!(resolved[1].contains("(volta)"), "{resolved:?}");

        let phantom = json!({ "resolved": null });
        let unprovisioned = probe_lines("pnpm", Some(r"C:\Volta\pnpm.EXE"), Some(&phantom));
        assert_eq!(unprovisioned.len(), 1);
        assert!(
            unprovisioned[0].contains("volta shim, not provisioned"),
            "{unprovisioned:?}"
        );
    }
}
