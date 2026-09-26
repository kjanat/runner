//! `runner doctor`, dump every signal the resolver considers.
//!
//! Surface for users (and bug reports) to inspect what runner sees in the current project:
//! detected package managers and task runners, the manifest declaration if any, lockfile presence,
//! override sources in effect, and the resolved decision. Pairs with `--explain` (one-line trace at run time)
//! and `runner why <task>` (per-task source pick).
//!
//! Two output formats:
//! - human (default): colored, grouped, easy to skim. Reads the flat [`Project`] shape internally (same one `list`/`info` serve).
//! - `--json`: the structured [`crate::schema::doctor::DoctorReport`], machine-readable JSON for piping into `jq`, scripts, or bug-report templates.

use anyhow::Result;
#[cfg(test)]
use serde_json::Value;

use crate::resolver::ResolutionOverrides;
use crate::schema::Project;
use crate::schema::doctor::DoctorReport;
use crate::types::ProjectContext;

/// Print a full diagnostic dump of the resolver's view of `ctx`.
///
/// # Errors
///
/// An observation failure is embedded in the report rather than propagated,
/// so this returns `Err` only when JSON serialization itself fails.
pub(crate) fn doctor(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    json: bool,
) -> Result<()> {
    if json {
        let report = DoctorReport::build(ctx, overrides, true);
        crate::render::json::print(&report)?;
        return Ok(());
    }

    let project = Project::build_with_schema(ctx, overrides, true);
    // The human renderer was written against a `serde_json::Value` so it
    // can address fields by name without a forest of `match`es.
    // Serializing the typed report once and traversing the resulting
    // `Value` keeps that ergonomics while the JSON contract itself stays
    // typed via `Project`.
    let report = serde_json::to_value(&project)?;
    // A plan that refuses to resolve (`on_collision = "error"`, an override
    // naming an undetected PM) is the diagnosis, so it is rendered rather than
    // propagated, same contract as the resolver error above.
    let plan = super::install::plan_install(ctx, overrides);
    crate::render::doctor::print_human(&crate::render::doctor::Human {
        report: &report,
        overrides,
        plan: plan.as_ref(),
        tools: super::install::tools_step(ctx, overrides, super::install::InstallFlags::default()),
        health: &crate::schema::doctor::provider_diagnostics(ctx, overrides),
    });

    Ok(())
}

/// Legacy stub retained for the existing tests that exercise
/// `build_report` directly. Pure passthrough to `Project::build` +
/// `serde_json::to_value`, same contract, same shape.
#[cfg(test)]
fn build_report(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> Value {
    serde_json::to_value(Project::build(ctx, overrides))
        .expect("Project must serialize for build_report")
}

#[cfg(test)]
mod tests {
    use super::{build_report, doctor};
    use crate::resolver::ResolutionOverrides;
    use crate::types::ProjectContext;
    use runner_core::ProviderId;

    fn context() -> ProjectContext {
        context_with(&[ProviderId::Pnpm, ProviderId::Cargo])
    }

    fn context_with(pms: &[ProviderId]) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        for pm in pms {
            crate::tool::test_support::write_signal(&root, *pm);
        }
        let mut ctx = ProjectContext {
            cwd: root.clone(),
            root,
            tasks: Vec::new(),
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        };
        crate::tool::test_support::seed_context(&mut ctx);
        ctx
    }

    #[test]
    fn build_report_omits_shims_when_not_resolving() {
        let ctx = context();
        let report = build_report(&ctx, &ResolutionOverrides::default());
        let signals = &report["signals"]["package.json"];

        assert!(
            signals.get("shims").is_none(),
            "shims must be omitted when empty: {signals}",
        );
        assert!(signals.get("path_probe").is_some(), "{signals}");
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
    fn package_json_is_dispatched_without_a_lockfile() {
        use std::fs;

        use crate::detect::detect;
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("doctor-node-context");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "build": "tsc" } }"#,
        )
        .expect("package.json should be written");
        let ctx = detect(dir.path(), &ResolutionOverrides::default());
        assert!(
            ctx.package_managers().is_empty(),
            "precondition: no lockfile-detected package manager"
        );

        let report = build_report(&ctx, &ResolutionOverrides::default());
        assert!(report["signals"].get("package.json").is_some(), "{report}");
        assert!(
            report["decisions"].get("package.json").is_some(),
            "{report}"
        );
    }

    #[test]
    fn a_rust_project_dispatches_no_managed_source() {
        let ctx = context_with(&[ProviderId::Cargo]);

        let report = build_report(&ctx, &ResolutionOverrides::default());
        assert_eq!(report["signals"], serde_json::json!({}));
        assert_eq!(report["decisions"], serde_json::json!({}));
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

        // When the manifest declaration disagrees with the detected lockfile,
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

        let ctx = detect(dir.path(), &ResolutionOverrides::default());
        let report = build_report(&ctx, &ResolutionOverrides::default());

        let warnings = report["warnings"].as_array().expect("warnings array");
        assert!(
            warnings.iter().any(|w| w["detail"]
                .as_str()
                .is_some_and(|d| d.contains("declaration wins"))),
            "expected resolver-produced PM mismatch warning to surface in doctor output, got: \
             {warnings:?}",
        );
    }
}
