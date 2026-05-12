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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use serde_json::{Map, Value, json};

use crate::resolver::{FallbackPolicy, OverrideOrigin, ResolutionOverrides, Resolver};
use crate::tool::node::{ManifestSource, detect_pm_from_manifest};
use crate::types::{Ecosystem, PackageManager, ProjectContext};

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
    let report = build_report(ctx, overrides);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report, overrides);
    }

    Ok(())
}

/// Build the structured report (`serde_json::Value` so the same data can
/// drive both renderers without round-trips through typed schema structs).
fn build_report(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> Value {
    let manifest_decl = detect_pm_from_manifest(&ctx.root);

    let node_signals = json!({
        "lockfile_pm": ctx.primary_node_pm().map(PackageManager::label),
        "manifest_pm": manifest_decl.as_ref().map(|d| json!({
            "pm": d.pm.label(),
            "source": match d.source {
                ManifestSource::PackageManager => "packageManager",
                ManifestSource::DevEngines => "devEngines.packageManager",
            },
            "version": d.version,
            "on_fail": format!("{:?}", d.on_fail).to_lowercase(),
        })),
        "path_probe": path_probe_map(),
    });

    let mut signals = Map::new();
    signals.insert("node".to_string(), node_signals);

    // Capture resolver-side warnings alongside the decision so they
    // surface in `doctor` output — CodeRabbit flagged that dropping
    // them hides actionable diagnostics. Manifest/lockfile-mismatch
    // and devEngines onFail=warn paths both produce these, and they're
    // exactly the signals a user runs `doctor` for.
    let (decisions, resolver_warnings) =
        match Resolver::new(ctx, overrides.clone()).resolve_node_pm() {
            Ok(decision) => {
                let warnings = decision.warnings.clone();
                (
                    json!({
                        "node_pm": {
                            "pm": decision.pm.label(),
                            "via": decision.describe(),
                        }
                    }),
                    warnings,
                )
            }
            Err(err) => (json!({ "node_pm_error": format!("{err}") }), Vec::new()),
        };

    let warnings_json: Vec<Value> = ctx
        .warnings
        .iter()
        .chain(resolver_warnings.iter())
        .map(|w| json!({ "source": w.source, "detail": w.detail }))
        .collect();

    json!({
        "schema_version": 1,
        "root": ctx.root.display().to_string(),
        "ecosystems": ctx.package_managers.iter().map(|pm| ecosystem_label(pm.ecosystem())).collect::<Vec<_>>(),
        "detected": {
            "package_managers": ctx.package_managers.iter().map(|pm| pm.label()).collect::<Vec<_>>(),
            "task_runners": ctx.task_runners.iter().map(|tr| tr.label()).collect::<Vec<_>>(),
            "node_version": ctx.node_version.as_ref().map(|nv| json!({
                "expected": nv.expected,
                "source": nv.source,
            })),
            "current_node": ctx.current_node,
            "monorepo": ctx.is_monorepo,
        },
        "overrides": overrides_json(overrides),
        "signals": signals,
        "decisions": decisions,
        "warnings": warnings_json,
    })
}

fn overrides_json(overrides: &ResolutionOverrides) -> Value {
    let mut pm_by_eco = Map::new();
    for (eco, pm_override) in &overrides.pm_by_ecosystem {
        pm_by_eco.insert(
            ecosystem_label(*eco).to_string(),
            json!({
                "pm": pm_override.pm.label(),
                "origin": origin_label(&pm_override.origin),
            }),
        );
    }

    json!({
        "pm": overrides.pm.as_ref().map(|o| json!({
            "pm": o.pm.label(),
            "origin": origin_label(&o.origin),
        })),
        "pm_by_ecosystem": pm_by_eco,
        "runner": overrides.runner.as_ref().map(|o| json!({
            "runner": o.runner.label(),
            "origin": origin_label(&o.origin),
        })),
        "fallback": fallback_label(overrides.fallback),
        "explain": overrides.explain,
    })
}

fn origin_label(origin: &OverrideOrigin) -> String {
    match origin {
        OverrideOrigin::CliFlag => "cli".to_string(),
        OverrideOrigin::EnvVar => "env".to_string(),
        OverrideOrigin::ConfigFile { path } => format!("config:{}", path.display()),
    }
}

const fn fallback_label(policy: FallbackPolicy) -> &'static str {
    match policy {
        FallbackPolicy::Probe => "probe",
        FallbackPolicy::Npm => "npm",
        FallbackPolicy::Error => "error",
    }
}

const fn ecosystem_label(eco: Ecosystem) -> &'static str {
    match eco {
        Ecosystem::Node => "node",
        Ecosystem::Deno => "deno",
        Ecosystem::Python => "python",
        Ecosystem::Rust => "rust",
        Ecosystem::Go => "go",
        Ecosystem::Ruby => "ruby",
        Ecosystem::Php => "php",
    }
}

/// Probe each Node PM in canonical order and report (binary, path) pairs.
/// Used for the doctor signals section; intentionally calls the real probe
/// so output reflects what the resolver would see.
fn path_probe_map() -> BTreeMap<&'static str, Option<String>> {
    use std::env;

    let path = env::var_os("PATH").unwrap_or_default();
    let pathext = env::var_os("PATHEXT");
    let probe = |name: &str| {
        crate::resolver::probe_path_for_doctor(name, &path, pathext.as_deref())
            .map(|p| p.display().to_string())
    };

    let mut map = BTreeMap::new();
    for pm in [
        PackageManager::Bun,
        PackageManager::Pnpm,
        PackageManager::Yarn,
        PackageManager::Npm,
    ] {
        map.insert(pm.label(), probe(pm.label()));
    }
    map
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
        if let Some(pm) = report["decisions"]["node_pm"].as_object() {
            writeln_field(out, "node scripts", pm["via"].as_str().unwrap_or("?"));
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

/// Stable Path that doctor uses to attribute "this is the project root".
/// Exposed for `--json` consumers that want to canonicalize the result.
#[allow(dead_code, reason = "kept as a thin abstraction layer for stability")]
pub(crate) fn root_path(ctx: &ProjectContext) -> &Path {
    &ctx.root
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
