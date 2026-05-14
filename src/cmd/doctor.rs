//! `runner doctor` — dump every signal the resolver considers.
//!
//! Surface for users (and bug reports) to inspect what runner sees in the
//! current project: detected package managers and task runners, the
//! manifest declaration if any, lockfile presence, override sources in
//! effect, and the resolved decision. Pairs with `--explain` (one-line
//! trace at run time) and `runner why <task>` (per-task source pick).
//!
//! Two output formats:
//! - human (default): colored, grouped, easy to skim.
//! - `--json`: machine-readable schema-versioned JSON for piping into
//!   `jq`, scripts, or bug-report templates.

use std::fmt::Write as _;

use anyhow::Result;
use colored::Colorize;
use serde_json::{Map, Value};

use crate::report::Project;
use crate::resolver::ResolutionOverrides;
use crate::types::ProjectContext;

/// Print a full diagnostic dump of the resolver's view of `ctx`.
///
/// # Errors
///
/// Propagates `Resolver::resolve_node_pm` errors when the configured
/// fallback policy is `error` and nothing is on `$PATH`. Always succeeds
/// for the `probe`/`npm` fallback policies on real systems.
pub(crate) fn doctor(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    json: bool,
) -> Result<()> {
    let project = Project::build(ctx, overrides);

    if json {
        println!("{}", serde_json::to_string_pretty(&project)?);
    } else {
        // The human renderer was written against a `serde_json::Value`
        // so it can address fields by name without a forest of
        // `match`es. Serializing the typed report once and traversing
        // the resulting `Value` keeps that ergonomics while the JSON
        // contract itself stays typed via `Project`.
        let report = serde_json::to_value(&project)?;
        print_human(&report, overrides);
    }

    Ok(())
}

/// Legacy stub retained for the existing tests that exercise
/// `build_report` directly. Pure passthrough to `Project::build` +
/// `serde_json::to_value` — same contract, same shape.
#[cfg(test)]
fn build_report(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> Value {
    serde_json::to_value(Project::build(ctx, overrides))
        .expect("Project must serialize for build_report")
}

#[allow(
    clippy::too_many_lines,
    reason = "linear section-by-section renderer; splitting hurts readability"
)]
fn print_human(report: &Value, overrides: &ResolutionOverrides) {
    let root = report["root"].as_str().unwrap_or("?");
    println!(
        "{} {}",
        "runner doctor".bold(),
        format!("@ {root}").dimmed()
    );
    println!();

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

    print_section("Signals (Node)", |out| {
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
            let parts: Vec<String> = probe
                .iter()
                .map(|(bin, path)| {
                    let val = path
                        .as_str()
                        .map_or_else(|| "not found".dimmed().to_string(), ToOwned::to_owned);
                    format!("{bin}={val}")
                })
                .collect();
            writeln_field(out, "PATH probe", &parts.join(", "));
        }
    });

    print_section("Decisions", |out| {
        // `Map<String, Value>` indexes panic on missing keys (unlike
        // `Value` indexing, which yields `Null`). Use `.get` so a
        // `node_pm` decision missing its `via` field renders `?`
        // instead of crashing the renderer.
        if let Some(pm) = report["decisions"]["node_pm"].as_object() {
            let via = pm.get("via").and_then(Value::as_str).unwrap_or("?");
            writeln_field(out, "node scripts", via);
        }
        if let Some(err) = report["decisions"]["node_pm_error"].as_str() {
            writeln!(out, "  {:<20}{}", "node scripts".red(), err.red())
                .expect("writeln to String should not fail");
        }
    });

    let warnings = report["warnings"].as_array().cloned().unwrap_or_default();
    if !warnings.is_empty() {
        println!("{}", "Warnings".bold());
        for w in &warnings {
            println!(
                "  {} {}: {}",
                "warn:".yellow().bold(),
                w["source"].as_str().unwrap_or("?"),
                w["detail"].as_str().unwrap_or("?"),
            );
        }
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
    use std::path::PathBuf;

    use super::{build_report, doctor};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{PackageManager, ProjectContext};

    fn context() -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("/tmp/test"),
            package_managers: vec![PackageManager::Pnpm, PackageManager::Cargo],
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn build_report_includes_schema_version() {
        let ctx = context();
        let report = build_report(&ctx, &ResolutionOverrides::default());

        assert_eq!(report["schema_version"], 1);
    }

    #[test]
    fn build_report_enumerates_detected_pms() {
        let ctx = context();
        let report = build_report(&ctx, &ResolutionOverrides::default());

        let pms = report["detected"]["package_managers"]
            .as_array()
            .expect("array");
        let labels: Vec<&str> = pms.iter().filter_map(|v| v.as_str()).collect();
        assert!(labels.contains(&"pnpm"));
        assert!(labels.contains(&"cargo"));
    }

    #[test]
    fn build_report_reports_ecosystems_from_detected_pms() {
        let ctx = context();
        let report = build_report(&ctx, &ResolutionOverrides::default());

        let ecos = report["ecosystems"].as_array().expect("array");
        let labels: Vec<&str> = ecos.iter().filter_map(|v| v.as_str()).collect();
        assert!(labels.contains(&"node"));
        assert!(labels.contains(&"rust"));
    }

    #[test]
    fn doctor_json_runs_without_panic() {
        let ctx = context();
        // Ensure both rendering paths are exercised; output goes to stdout
        // which is fine in tests (captured by `cargo test`).
        doctor(&ctx, &ResolutionOverrides::default(), true).expect("json render should succeed");
        doctor(&ctx, &ResolutionOverrides::default(), false).expect("human render should succeed");
    }

    #[test]
    fn build_report_merges_resolver_warnings_with_ctx_warnings() {
        use std::fs;

        use crate::detect::detect;
        use crate::tool::test_support::TempDir;

        // Manifest declaration disagrees with the detected lockfile —
        // the resolver emits a `package.json` warning. Doctor should
        // surface it alongside whatever ctx.warnings already carries.
        let dir = TempDir::new("doctor-merges-warnings");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "yarn@4.3.0" }"#,
        )
        .expect("package.json should be written");
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 9\n")
            .expect("pnpm-lock.yaml should be written");

        let ctx = detect(dir.path());
        let report = build_report(&ctx, &ResolutionOverrides::default());

        let warnings = report["warnings"].as_array().expect("warnings array");
        assert!(
            warnings.iter().any(|w| w["detail"]
                .as_str()
                .is_some_and(|d| d.contains("declaration wins"))),
            "expected resolver-produced PM mismatch warning to surface in doctor output, got: {warnings:?}",
        );
    }
}
