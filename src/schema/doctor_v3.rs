//! `doctor --json` schema **v3** — the structured diagnostic report.
//!
//! Implements the contract drafted in `schemas/doctor.v3-draft.schema.json`
//! (now retired): instead of v2's flat detection dump, the report is an
//! inventory — `invocation`/`environment`/`runner` provenance, per-ecosystem
//! decisions with confidence, task `sources` as first-class objects, tasks
//! with stable `fqn`s, PATH-probe `tools`, duplicate-name `conflicts`, and
//! flattened `diagnostics` — plus a self-describing `resolution` policy
//! block.
//!
//! Deliberate deltas from the draft, found while reviewing it against the
//! codebase:
//!
//! - `tasks[].resolved` and `tasks[].source` are nullable: a
//!   `package.json` script's command depends on PM resolution, which can
//!   fail, and a source anchor file can be undiscoverable. The draft
//!   required both non-null; lying was the alternative.
//! - `sources[].kind` uses the v3 source labels (`cargo-alias`, `just`,
//!   …) for cross-surface consistency with `why` v3, not the draft's
//!   filename-flavored examples (`cargo-config`, `justfile`).
//! - `overrides.pm`/`overrides.runner` are bare labels (per draft); the
//!   provenance (`cli`/`env`/`config:…`) remains available on the v2
//!   surface.
//! - `project.workspace` is always `null` and `project.root_source` is
//!   the root itself until workspace/root-anchor detection is modeled.
//! - Speculative draft shapes nothing can emit yet are deferred rather
//!   than declared: the rich `dependency` object (`tasks[].dependencies`
//!   stays an always-empty array), `workspace`/`package_identity`
//!   objects (fields stay null), the `tool_probe_error` variant (the
//!   probe cannot error), the `binary`/`package-binary` tool kinds, and
//!   the `debug`/`error` severities. Each gets declared when an
//!   emitter exists — contracts should describe output, not ambition.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::labels::source_label_for;
use crate::cmd::run::{resolve_python_pm, select_task_entry, source_depth, source_priority};
use crate::resolver::{
    FallbackPolicy, MismatchPolicy, ResolutionOverrides, ResolutionStep, Resolver,
};
use crate::tool::node::detect_pm_from_manifest;
use crate::types::{DetectionWarning, Ecosystem, PackageManager, ProjectContext, Task, TaskSource};

/// `runner doctor --json --schema-version 3` payload.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub(crate) struct DoctorReportV3<'a> {
    #[serde(rename = "$schema")]
    #[cfg_attr(
        feature = "schema",
        schemars(description = "URI of the JSON Schema that describes this payload.")
    )]
    schema: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Schema contract version for this JSON payload.")
    )]
    schema_version: u32,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Payload discriminator; always \"runner.doctor\".")
    )]
    kind: &'static str,
    invocation: InvocationV3,
    environment: EnvironmentV3,
    runner: RunnerInfoV3,
    project: ProjectInfoV3,
    overrides: OverridesV3,
    ecosystems: Vec<EcosystemV3>,
    sources: Vec<SourceV3>,
    tasks: Vec<DoctorTaskV3<'a>>,
    tools: Vec<ToolV3>,
    conflicts: Vec<ConflictV3>,
    diagnostics: Vec<DiagnosticV3>,
    resolution: ResolutionPolicyV3,
}

/// How this report came to be: the exact process invocation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct InvocationV3 {
    argv: Vec<String>,
    cwd: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "UTC RFC 3339 timestamp of report generation.")
    )]
    started_at: String,
}

/// Host facts that influence probing and dispatch.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct EnvironmentV3 {
    arch: &'static str,
    os: &'static str,
    path_entries: Vec<String>,
    shell: Option<String>,
}

/// The reporting binary's own identity and contract versions.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct RunnerInfoV3 {
    binary: String,
    name: String,
    version: &'static str,
    schema_versions: SchemaVersionsV3,
}

/// Latest schema version each `--json` surface speaks.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct SchemaVersionsV3 {
    doctor: u32,
    list: u32,
    why: u32,
}

/// Project anchoring facts.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct ProjectInfoV3 {
    monorepo: bool,
    root: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "What anchored root detection. Currently always the root itself (cwd or --dir); a dedicated anchor model is future work."
        )
    )]
    root_source: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Workspace identity. Always null today: workspace kind/root detection is not yet modeled (the monorepo flag is the coarse signal)."
        )
    )]
    workspace: Option<serde_json::Value>,
}

/// Effective override stack, labels only. Provenance (cli/env/config)
/// stays on the v2 surface.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct OverridesV3 {
    explain: bool,
    fallback: &'static str,
    no_warnings: bool,
    quiet: bool,
    on_mismatch: &'static str,
    pm: Option<&'static str>,
    pm_by_ecosystem: BTreeMap<String, Option<&'static str>>,
    prefer_runners: Vec<&'static str>,
    runner: Option<&'static str>,
}

/// One detected ecosystem and the PM decision made for it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct EcosystemV3 {
    decision: EcosystemDecisionV3,
    name: &'static str,
    root: String,
    selected_package_manager: Option<&'static str>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Detection evidence. Node carries the full signal set (lockfile/manifest/PATH probe/shim classification, keyed by tool with the shim manager as data); other ecosystems list their detected package managers."
        )
    )]
    signals: serde_json::Value,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct EcosystemDecisionV3 {
    confidence: ConfidenceV3,
    reason: String,
    selected: Option<&'static str>,
}

/// How sure the resolver is about an ecosystem's PM selection.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum ConfidenceV3 {
    /// Explicit signal: override, manifest declaration, or lockfile.
    High,
    /// Inferred: PATH probe found a usable binary.
    Medium,
    /// Legacy `--fallback npm` default with no signal at all.
    Low,
    /// Resolution failed.
    None,
}

/// One task-source config file as a first-class object.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct SourceV3 {
    exists: bool,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Stable source identity: `src:<scope>:<kind>`.")
    )]
    id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "v3 source label (same convention as `why` v3).")
    )]
    kind: &'static str,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Package identity for manifest-backed sources. Null today.")
    )]
    package: Option<serde_json::Value>,
    path: String,
    relpath: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Project-root-relative scope; `root` until member scoping lands.")
    )]
    scope: &'static str,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Key of the container holding tasks inside the file (`scripts`, `tasks`, `alias`, …); null for flat-format files."
        )
    )]
    task_pointer: Option<&'static str>,
}

/// One task in the doctor inventory. Same identity scheme as `why` v3
/// (`fqn`, `source_pointer`, `aliases`, `definition`, `resolved`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct DoctorTaskV3<'a> {
    aliases: Vec<&'a str>,
    cwd: String,
    definition: Option<&'a str>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Task dependencies. Always empty today: no extractor records dependency edges yet; the edge shape lands with the first extractor."
        )
    )]
    dependencies: Vec<serde_json::Value>,
    description: Option<&'a str>,
    fqn: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "True when this task is an alias for another target; `definition` holds the target it expands to (e.g. cargo `b` → `build`)."
        )
    )]
    is_alias: bool,
    name: &'a str,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Effective command preview. Null when it depends on a PM resolution that failed."
        )
    )]
    resolved: Option<String>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "True when runner can run this task without its source's primary tool. Only deno tasks runner can execute via the embedded task shell (leaf command, no `dependencies`, no `deno` invocation) qualify today; all other sources are false."
        )
    )]
    self_executable: bool,
    source: Option<String>,
    source_pointer: Option<String>,
}

/// What kind of thing a probed tool is. The draft's `binary` /
/// `package-binary` kinds join when something probes them.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DependencyKindV3 {
    Runtime,
    PackageManager,
    TaskRunner,
}

impl DependencyKindV3 {
    const fn label(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::PackageManager => "package-manager",
            Self::TaskRunner => "task-runner",
        }
    }
}

/// One PATH-probed tool the project relies on.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct ToolV3 {
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Stable tool identity: `tool:<kind>:<name>`.")
    )]
    id: String,
    kind: DependencyKindV3,
    name: &'static str,
    probe: ToolProbeV3,
    required: bool,
}

/// PATH-probe outcome, tagged by `status`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
enum ToolProbeV3 {
    Found {
        path: String,
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "Resolved version: taken from detection when known, otherwise read by running `<binary> --version`. Null when the binary reports no parseable version."
            )
        )]
        version: Option<String>,
    },
    Missing,
}

/// A task name claimed by more than one source: who wins, who is shadowed.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct ConflictV3 {
    kind: &'static str,
    reason: String,
    #[cfg_attr(feature = "schema", schemars(description = "FQN of the winning task."))]
    selected: String,
    selector: String,
    severity: SeverityV3,
    shadowed: Vec<String>,
}

/// Severity of a conflict or diagnostic. The draft's `debug`/`error`
/// levels join when something emits them.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum SeverityV3 {
    Info,
    Warning,
}

/// One detection/resolution diagnostic, flattened from the warning
/// streams.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct DiagnosticV3 {
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Stable warning category (the warning's source subsystem).")
    )]
    code: &'static str,
    message: String,
    severity: SeverityV3,
    source: Option<&'static str>,
    task: Option<String>,
}

/// Self-description of the task-selection policy, so consumers don't
/// hardcode runner's precedence rules.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct ResolutionPolicyV3 {
    fqn_policy: &'static str,
    precedence: Vec<&'static str>,
    short_name_policy: &'static str,
}

impl<'a> DoctorReportV3<'a> {
    /// Build the v3 report. `resolve_shims` is forwarded to the Volta
    /// shim classifier exactly like the v2 builder.
    pub(crate) fn build(
        ctx: &'a ProjectContext,
        overrides: &ResolutionOverrides,
        resolve_shims: bool,
    ) -> Self {
        let node_pm = Resolver::new(ctx, overrides).resolve_node_pm();
        let schema_version = super::DOCTOR_CURRENT_VERSION;

        let diagnostics = ctx
            .warnings
            .iter()
            .chain(node_pm.as_ref().map_or(&[][..], |d| &d.warnings))
            .map(diagnostic_v3)
            .collect();

        Self {
            schema: super::schema_url("doctor", schema_version),
            schema_version,
            kind: "runner.doctor",
            invocation: invocation_v3(),
            environment: environment_v3(),
            runner: runner_info_v3(),
            project: ProjectInfoV3 {
                monorepo: ctx.is_monorepo,
                root: ctx.root.display().to_string(),
                root_source: ctx.root.display().to_string(),
                workspace: None,
            },
            overrides: overrides_v3(overrides),
            ecosystems: ecosystems_v3(ctx, overrides, &node_pm, resolve_shims),
            sources: sources_v3(ctx, schema_version),
            tasks: tasks_v3(ctx, &node_pm, overrides, schema_version),
            tools: tools_v3(ctx, &node_pm),
            conflicts: conflicts_v3(ctx, overrides, schema_version),
            diagnostics,
            resolution: ResolutionPolicyV3 {
                fqn_policy: "exact-only",
                precedence: vec![
                    "source-priority",
                    "source-depth",
                    "display-order",
                    "alias-last",
                ],
                short_name_policy: "deterministic-precedence",
            },
        }
    }
}

fn invocation_v3() -> InvocationV3 {
    InvocationV3 {
        argv: std::env::args().collect(),
        cwd: std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
        started_at: rfc3339_utc_now(),
    }
}

fn environment_v3() -> EnvironmentV3 {
    EnvironmentV3 {
        arch: std::env::consts::ARCH,
        os: std::env::consts::OS,
        path_entries: std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .map(|entry| entry.display().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        shell: std::env::var("SHELL").ok(),
    }
}

fn runner_info_v3() -> RunnerInfoV3 {
    let binary = std::env::current_exe()
        .map_or_else(|_| "runner".to_string(), |exe| exe.display().to_string());
    let name = std::env::args_os()
        .next()
        .and_then(|arg0| crate::bin_name_from_arg0(&arg0))
        .unwrap_or_else(|| "runner".to_string());
    RunnerInfoV3 {
        binary,
        name,
        version: env!("CARGO_PKG_VERSION"),
        schema_versions: SchemaVersionsV3 {
            doctor: super::DOCTOR_CURRENT_VERSION,
            list: super::CURRENT_VERSION,
            why: super::WHY_CURRENT_VERSION,
        },
    }
}

fn overrides_v3(overrides: &ResolutionOverrides) -> OverridesV3 {
    OverridesV3 {
        explain: overrides.explain,
        fallback: match overrides.fallback {
            FallbackPolicy::Probe => "probe",
            FallbackPolicy::Npm => "npm",
            FallbackPolicy::Error => "error",
        },
        no_warnings: overrides.no_warnings,
        quiet: overrides.quiet,
        on_mismatch: match overrides.on_mismatch {
            MismatchPolicy::Warn => "warn",
            MismatchPolicy::Error => "error",
            MismatchPolicy::Ignore => "ignore",
        },
        pm: overrides.pm.as_ref().map(|o| o.pm.label()),
        pm_by_ecosystem: overrides
            .pm_by_ecosystem
            .iter()
            .map(|(eco, o)| (eco.label().to_string(), Some(o.pm.label())))
            .collect(),
        prefer_runners: overrides.prefer_runners.iter().map(|r| r.label()).collect(),
        runner: overrides.runner.as_ref().map(|o| o.runner.label()),
    }
}

fn ecosystems_v3(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    resolve_shims: bool,
) -> Vec<EcosystemV3> {
    let mut seen = Vec::new();
    for pm in &ctx.package_managers {
        let eco = pm.ecosystem();
        if !seen.contains(&eco) {
            seen.push(eco);
        }
    }

    // Seeding from detected `package_managers` alone misses every Node
    // resolution that doesn't leave a lockfile-detected PM behind
    // (manifest `packageManager` without a lockfile, PATH-probe, npm
    // fallback, override). In those cases `tasks_v3` still resolves
    // `package.json` scripts via `npm run`, so dropping Node here would
    // emit an internally inconsistent document. Same predicate gates the
    // node runtime entry in `tools_v3`.
    if has_node_context(ctx, node_pm) && !seen.contains(&Ecosystem::Node) {
        seen.push(Ecosystem::Node);
    }

    seen.into_iter()
        .map(|eco| match eco {
            Ecosystem::Node => node_ecosystem_v3(ctx, node_pm, resolve_shims),
            Ecosystem::Python => python_ecosystem_v3(ctx, overrides),
            other => single_pm_ecosystem_v3(ctx, other),
        })
        .collect()
}

/// Whether the project carries Node context, considering resolver and
/// task signals — not just lockfile-detected `package_managers`. A Node
/// PM decision (`resolve_node_pm` Ok), a detected Node-ecosystem PM, or
/// any `package.json`-sourced task each count. Gates Node inclusion in
/// both [`ecosystems_v3`] and [`tools_v3`] so the two surfaces never
/// disagree with what `tasks_v3` resolves.
fn has_node_context(
    ctx: &ProjectContext,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
) -> bool {
    node_pm.is_ok()
        || ctx
            .package_managers
            .iter()
            .any(|pm| pm.ecosystem() == Ecosystem::Node)
        || ctx
            .tasks
            .iter()
            .any(|t| matches!(t.source, TaskSource::PackageJson))
}

fn node_ecosystem_v3(
    ctx: &ProjectContext,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    resolve_shims: bool,
) -> EcosystemV3 {
    let (decision, selected) = match node_pm {
        Ok(decision) => (
            EcosystemDecisionV3 {
                confidence: confidence_for_step(&decision.via),
                reason: decision.describe(),
                selected: Some(decision.pm.label()),
            },
            Some(decision.pm.label()),
        ),
        Err(err) => (
            EcosystemDecisionV3 {
                confidence: ConfidenceV3::None,
                reason: format!("{err}"),
                selected: None,
            },
            None,
        ),
    };

    let manifest_decl = detect_pm_from_manifest(&ctx.root);
    let probes = super::project::probe_signals(&ctx.root, resolve_shims);
    // Shims are keyed by tool and carry the shim *manager* as data, not
    // as the field name — Volta is merely the first manager the prober
    // classifies; asdf/mise/proto entries slot in without a contract
    // change. (v2's `volta_shims` spelling is frozen; only v3 gets the
    // generic shape.)
    let shims = probes
        .volta_shims
        .iter()
        .map(|(name, shim)| {
            (
                (*name).to_string(),
                serde_json::json!({ "manager": "volta", "resolved": shim.resolved }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let signals = serde_json::json!({
        "lockfile_pm": ctx.primary_node_pm().map(PackageManager::label),
        "manifest_pm": manifest_decl.as_ref().map(|d| d.pm.label()),
        "path_probe": probes.path_probe,
        "shims": shims,
    });

    EcosystemV3 {
        decision,
        name: "node",
        root: ctx.root.display().to_string(),
        selected_package_manager: selected,
        signals,
    }
}

fn python_ecosystem_v3(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> EcosystemV3 {
    let resolved = resolve_python_pm(ctx, overrides);
    let (decision, selected) = resolved.map_or_else(
        || {
            (
                EcosystemDecisionV3 {
                    confidence: ConfidenceV3::None,
                    reason: "no Python package manager detected".to_string(),
                    selected: None,
                },
                None,
            )
        },
        |decision| {
            let label = decision.pm.label();
            (
                EcosystemDecisionV3 {
                    confidence: ConfidenceV3::High,
                    reason: decision.describe(),
                    selected: Some(label),
                },
                Some(label),
            )
        },
    );

    EcosystemV3 {
        decision,
        name: "python",
        root: ctx.root.display().to_string(),
        selected_package_manager: selected,
        signals: detected_pm_signals(ctx, Ecosystem::Python),
    }
}

/// Single-PM ecosystems (rust/go/deno/ruby/php): the detected manager
/// *is* the decision — there is no competing-PM resolution chain.
fn single_pm_ecosystem_v3(ctx: &ProjectContext, eco: Ecosystem) -> EcosystemV3 {
    let selected = ctx
        .package_managers
        .iter()
        .find(|pm| pm.ecosystem() == eco)
        .map(|pm| pm.label());

    EcosystemV3 {
        decision: EcosystemDecisionV3 {
            confidence: ConfidenceV3::High,
            reason: format!(
                "detected via {} project signal",
                selected.unwrap_or("manifest")
            ),
            selected,
        },
        name: eco.label(),
        root: ctx.root.display().to_string(),
        selected_package_manager: selected,
        signals: detected_pm_signals(ctx, eco),
    }
}

fn detected_pm_signals(ctx: &ProjectContext, eco: Ecosystem) -> serde_json::Value {
    serde_json::json!({
        "package_managers": ctx
            .package_managers
            .iter()
            .filter(|pm| pm.ecosystem() == eco)
            .map(|pm| pm.label())
            .collect::<Vec<_>>(),
    })
}

const fn confidence_for_step(step: &ResolutionStep) -> ConfidenceV3 {
    match step {
        ResolutionStep::Override(_)
        | ResolutionStep::ManifestPackageManager
        | ResolutionStep::ManifestDevEngines { .. }
        | ResolutionStep::Lockfile => ConfidenceV3::High,
        ResolutionStep::PathProbe { .. } => ConfidenceV3::Medium,
        ResolutionStep::LegacyNpmFallback => ConfidenceV3::Low,
    }
}

fn sources_v3(ctx: &ProjectContext, schema_version: u32) -> Vec<SourceV3> {
    let mut seen: Vec<TaskSource> = Vec::new();
    for task in &ctx.tasks {
        if !seen.contains(&task.source) {
            seen.push(task.source);
        }
    }

    seen.into_iter()
        .map(|source| {
            let kind = source_label_for(source, schema_version);
            let anchor = anchor_file(source, &ctx.root);
            let path = anchor
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string());
            let relpath = anchor.as_ref().map_or_else(String::new, |p| {
                p.strip_prefix(&ctx.root).unwrap_or(p).display().to_string()
            });
            SourceV3 {
                exists: anchor.as_ref().is_some_and(|p| p.is_file()),
                id: format!("src:root:{kind}"),
                kind,
                package: None,
                path,
                relpath,
                scope: "root",
                task_pointer: task_container_key(source),
            }
        })
        .collect()
}

fn tasks_v3<'a>(
    ctx: &'a ProjectContext,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    overrides: &ResolutionOverrides,
    schema_version: u32,
) -> Vec<DoctorTaskV3<'a>> {
    let node_pm_label = node_pm.as_ref().ok().map(|d| d.pm.label());
    let python_pm_label = resolve_python_pm(ctx, overrides).map(|d| d.pm.label());

    // `anchor_file` walks the filesystem; resolve each distinct source
    // once instead of once per task.
    let mut anchors: std::collections::HashMap<TaskSource, Option<String>> =
        std::collections::HashMap::new();
    for task in &ctx.tasks {
        anchors.entry(task.source).or_insert_with(|| {
            anchor_file(task.source, &ctx.root).map(|p| p.display().to_string())
        });
    }

    ctx.tasks
        .iter()
        .map(|task| DoctorTaskV3 {
            aliases: ctx
                .tasks
                .iter()
                .filter(|other| {
                    other.source == task.source && other.alias_of.as_deref() == Some(&task.name)
                })
                .map(|other| other.name.as_str())
                .collect(),
            cwd: ctx.root.display().to_string(),
            definition: task.alias_of.as_deref().or(task.run_target.as_deref()),
            dependencies: Vec::new(),
            description: task.description.as_deref(),
            fqn: super::labels::fqn(task.source, &task.name, schema_version),
            is_alias: task.alias_of.is_some(),
            name: &task.name,
            resolved: resolved_command_v3(task, node_pm_label, python_pm_label),
            self_executable: deno_task_self_executable(ctx, task),
            source: anchors.get(&task.source).cloned().flatten(),
            source_pointer: source_pointer_v3(task),
        })
        .collect()
}

/// Whether runner can run `task` without its source's primary tool.
///
/// Only deno tasks that runner can drive through the embedded task shell
/// (leaf command, no `dependencies`, no `deno` invocation) qualify; every
/// other source has no in-process fallback and is therefore `false`.
fn deno_task_self_executable(ctx: &ProjectContext, task: &Task) -> bool {
    if task.source != TaskSource::DenoJson {
        return false;
    }
    crate::tool::deno::find_config_upwards(&ctx.root)
        .and_then(|config| crate::tool::deno_exec::plan(&config, &task.name))
        .is_some_and(|plan| plan.self_executable())
}

/// Effective command preview. Unlike `why` v3 (which only resolves the
/// PM for the selected task), doctor resolves PMs project-wide, so
/// `package.json`/`pyproject.toml` scripts resolve here whenever the
/// ecosystem resolution succeeded.
fn resolved_command_v3(
    task: &Task,
    node_pm: Option<&'static str>,
    python_pm: Option<&'static str>,
) -> Option<String> {
    let name = &task.name;
    match task.source {
        TaskSource::CargoAliases => Some(task.alias_of.as_deref().map_or_else(
            || format!("cargo {name}"),
            |expansion| format!("cargo {expansion}"),
        )),
        TaskSource::DenoJson => Some(format!("deno task {name}")),
        TaskSource::TurboJson => Some(format!("turbo run {name}")),
        TaskSource::Makefile => Some(format!("make {name}")),
        TaskSource::Justfile => Some(format!("just {name}")),
        TaskSource::Taskfile => Some(format!("task {name}")),
        TaskSource::BaconToml => Some(format!("bacon {name}")),
        TaskSource::MiseToml => Some(format!("mise run {name}")),
        TaskSource::GoPackage => Some(format!(
            "go run {target}",
            target = task.run_target.as_deref().unwrap_or(name)
        )),
        TaskSource::PackageJson => node_pm.map(|pm| format!("{pm} run {name}")),
        TaskSource::PyprojectScripts => python_pm.map(|pm| format!("{pm} run {name}")),
    }
}

/// Key path locating the task inside its source file; mirrors the
/// `why` v3 convention.
fn source_pointer_v3(task: &Task) -> Option<String> {
    let name = &task.name;
    match task.source {
        TaskSource::CargoAliases => Some(format!("alias.{name}")),
        TaskSource::PackageJson => Some(format!("scripts.{name}")),
        TaskSource::DenoJson
        | TaskSource::TurboJson
        | TaskSource::Taskfile
        | TaskSource::MiseToml => Some(format!("tasks.{name}")),
        TaskSource::BaconToml => Some(format!("jobs.{name}")),
        TaskSource::PyprojectScripts => Some(format!("project.scripts.{name}")),
        TaskSource::Makefile | TaskSource::Justfile => Some(name.clone()),
        TaskSource::GoPackage => None,
    }
}

/// Container key holding tasks inside the source file.
const fn task_container_key(source: TaskSource) -> Option<&'static str> {
    match source {
        TaskSource::CargoAliases => Some("alias"),
        TaskSource::PackageJson => Some("scripts"),
        TaskSource::DenoJson
        | TaskSource::TurboJson
        | TaskSource::Taskfile
        | TaskSource::MiseToml => Some("tasks"),
        TaskSource::BaconToml => Some("jobs"),
        TaskSource::PyprojectScripts => Some("project.scripts"),
        TaskSource::Makefile | TaskSource::Justfile | TaskSource::GoPackage => None,
    }
}

/// Config file anchoring a task source. Mirrors `cmd::why`'s anchor
/// walk (file paths, not parent dirs).
fn anchor_file(source: TaskSource, root: &Path) -> Option<PathBuf> {
    use crate::tool;

    match source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(root),
        TaskSource::DenoJson => tool::deno::find_config_upwards(root),
        TaskSource::TurboJson => tool::turbo::find_config(root),
        TaskSource::Makefile => tool::files::find_first(root, tool::make::FILENAMES),
        TaskSource::Justfile => tool::just::find_file(root),
        TaskSource::Taskfile => tool::files::find_first(root, tool::go_task::FILENAMES),
        TaskSource::CargoAliases => tool::cargo_aliases::find_anchor(root),
        TaskSource::GoPackage => tool::go_pm::find_file(root),
        TaskSource::BaconToml => tool::files::find_first(root, tool::bacon::FILENAMES),
        TaskSource::MiseToml => tool::mise::find_file(root),
        TaskSource::PyprojectScripts => tool::python::find_pyproject_upwards(root),
    }
}

fn tools_v3(
    ctx: &ProjectContext,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
) -> Vec<ToolV3> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var_os("PATHEXT");
    let pathext_ref = pathext.as_deref();

    let mut tools = Vec::new();

    if has_node_context(ctx, node_pm) {
        tools.push(probe_tool(
            "node",
            DependencyKindV3::Runtime,
            ctx.current_node
                .as_deref()
                .map(|v| v.trim_start_matches('v').to_string()),
            true,
            &path,
            pathext_ref,
        ));
    }

    // Deno is required only when at least one deno task can't be
    // self-executed (it has dependencies or invokes `deno`); a project
    // whose deno tasks all run through the embedded shell does not need
    // the binary. Every other tool has no in-process fallback.
    let deno_required = ctx
        .tasks
        .iter()
        .filter(|task| task.source == TaskSource::DenoJson)
        .any(|task| !deno_task_self_executable(ctx, task));

    for pm in &ctx.package_managers {
        let required = if *pm == PackageManager::Deno {
            deno_required
        } else {
            true
        };
        tools.push(probe_tool(
            pm_binary_name(*pm),
            DependencyKindV3::PackageManager,
            None,
            required,
            &path,
            pathext_ref,
        ));
    }
    for runner in &ctx.task_runners {
        tools.push(probe_tool(
            runner.label(),
            DependencyKindV3::TaskRunner,
            None,
            true,
            &path,
            pathext_ref,
        ));
    }

    tools
}

/// Binary actually probed for a PM. Labels and binaries coincide except
/// Bundler, whose CLI is `bundle`.
const fn pm_binary_name(pm: PackageManager) -> &'static str {
    match pm {
        PackageManager::Bundler => "bundle",
        _ => pm.label(),
    }
}

fn probe_tool(
    name: &'static str,
    kind: DependencyKindV3,
    version: Option<String>,
    required: bool,
    path: &std::ffi::OsStr,
    pathext: Option<&std::ffi::OsStr>,
) -> ToolV3 {
    let probe = crate::resolver::probe_path_for_doctor(name, path, pathext).map_or(
        ToolProbeV3::Missing,
        |hit| ToolProbeV3::Found {
            // Prefer a version already known from detection (the node
            // runtime); otherwise ask the binary directly.
            version: version.or_else(|| probe_tool_version(&hit)),
            path: hit.display().to_string(),
        },
    );
    ToolV3 {
        id: format!("tool:{kind}:{name}", kind = kind.label()),
        kind,
        name,
        probe,
        required,
    }
}

/// Run `<binary> --version` and extract the version string. Returns
/// `None` when the spawn fails, the process errors, or no version-like
/// token appears. Output formats vary (`cargo 1.83.0 (..)`, `just
/// 1.36.0`, `1.1.38`, `v24.14.1`), so the first whitespace-separated
/// token that looks like a dotted version wins, with any `v` prefix
/// stripped.
fn probe_tool_version(binary: &Path) -> Option<String> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            // Version-like: starts with a digit and carries a dotted
            // component. Accepts `1.83.0`, `24.14.1`, and prerelease
            // forms like `1.85.0-nightly`; skips names and build hashes.
            token.starts_with(|c: char| c.is_ascii_digit()) && token.contains('.')
        })
        .map(ToString::to_string)
}

fn conflicts_v3(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    schema_version: u32,
) -> Vec<ConflictV3> {
    let mut by_name: BTreeMap<&str, Vec<&Task>> = BTreeMap::new();
    for task in &ctx.tasks {
        by_name.entry(&task.name).or_default().push(task);
    }

    by_name
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .map(|(name, group)| {
            let selected = select_task_entry(ctx, overrides, &group);
            let fqn_of = |task: &Task| super::labels::fqn(task.source, &task.name, schema_version);
            ConflictV3 {
                kind: "duplicate-task-name",
                reason: format!(
                    "{count} sources define `{name}`; lowest (source_priority={priority}, \
                     source_depth={depth}, display_order={order}, alias-last) key wins",
                    count = group.len(),
                    priority = source_priority(overrides, selected.source),
                    depth = display_depth(source_depth(ctx, selected.source)),
                    order = selected.source.display_order(),
                ),
                selected: fqn_of(selected),
                selector: name.to_string(),
                severity: SeverityV3::Info,
                shadowed: group
                    .iter()
                    .filter(|task| !std::ptr::eq(**task, selected))
                    .map(|task| fqn_of(task))
                    .collect(),
            }
        })
        .collect()
}

fn display_depth(depth: usize) -> String {
    if depth == usize::MAX {
        "unresolved".to_string()
    } else {
        depth.to_string()
    }
}

fn diagnostic_v3(warning: &DetectionWarning) -> DiagnosticV3 {
    DiagnosticV3 {
        code: warning.source(),
        message: warning.detail(),
        severity: SeverityV3::Warning,
        source: Some(warning.source()),
        task: None,
    }
}

/// RFC 3339 UTC timestamp without a date-time dependency. Civil-date
/// math per Howard Hinnant's `civil_from_days` algorithm.
fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    rfc3339_utc(secs)
}

fn rfc3339_utc(secs_since_epoch: u64) -> String {
    let days = i64::try_from(secs_since_epoch / 86_400).unwrap_or(i64::MAX);
    let rem = secs_since_epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z",
        hour = rem / 3600,
        minute = (rem % 3600) / 60,
        second = rem % 60,
    )
}

/// Days-since-epoch → (year, month, day) in the proleptic Gregorian
/// calendar. <https://howardhinnant.github.io/date_algorithms.html>
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DoctorReportV3, rfc3339_utc};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{Ecosystem, PackageManager, ProjectContext, Task, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("/tmp/test"),
            package_managers: vec![PackageManager::Cargo],
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn rfc3339_known_vectors() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(86_400), "1970-01-02T00:00:00Z");
        // 2000-02-29 — leap day in a century-leap year.
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(951_868_799), "2000-02-29T23:59:59Z");
        assert_eq!(rfc3339_utc(951_868_800), "2000-03-01T00:00:00Z");
    }

    #[test]
    fn v3_report_carries_contract_constants() {
        let ctx = context(vec![]);
        let report = DoctorReportV3::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["kind"], "runner.doctor");
        assert_eq!(json["schema_version"], 3);
        assert!(
            json["$schema"]
                .as_str()
                .is_some_and(|s| s.contains("doctor.v3"))
        );
        assert_eq!(json["resolution"]["fqn_policy"], "exact-only");
        assert_eq!(json["project"]["workspace"], serde_json::Value::Null);
        assert!(
            json["invocation"]["started_at"]
                .as_str()
                .is_some_and(|t| { t.len() == 20 && t.ends_with('Z') && t.as_bytes()[10] == b'T' })
        );
    }

    #[test]
    fn v3_report_lists_rust_ecosystem_with_high_confidence() {
        let ctx = context(vec![]);
        let report = DoctorReportV3::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let eco = &json["ecosystems"][0];
        assert_eq!(eco["name"], "rust");
        assert_eq!(eco["selected_package_manager"], "cargo");
        assert_eq!(eco["decision"]["confidence"], "high");
    }

    #[test]
    fn v3_report_surfaces_duplicate_names_as_conflicts() {
        let mut alias = task("t", TaskSource::CargoAliases);
        alias.alias_of = Some("test".to_string());
        let ctx = context(vec![alias, task("t", TaskSource::Justfile)]);
        let report = DoctorReportV3::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let conflict = &json["conflicts"][0];
        assert_eq!(conflict["kind"], "duplicate-task-name");
        assert_eq!(conflict["selector"], "t");
        // The justfile recipe wins: same tier, but recipes rank before
        // aliases.
        assert_eq!(conflict["selected"], "root:just#t");
        assert_eq!(
            conflict["shadowed"],
            serde_json::json!(["root:cargo-alias#t"])
        );
    }

    #[test]
    fn v3_report_resolves_cargo_alias_tasks() {
        let mut alias = task("t", TaskSource::CargoAliases);
        alias.alias_of = Some("test".to_string());
        let ctx = context(vec![alias]);
        let report = DoctorReportV3::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let task = &json["tasks"][0];
        assert_eq!(task["fqn"], "root:cargo-alias#t");
        assert_eq!(task["is_alias"], true);
        assert_eq!(task["definition"], "test");
        assert_eq!(task["resolved"], "cargo test");
        assert_eq!(task["source_pointer"], "alias.t");
        assert_eq!(task["dependencies"], serde_json::json!([]));
    }

    #[test]
    fn v3_report_keeps_node_when_only_package_json_tasks_present() {
        // package.json scripts with no lockfile-detected Node PM: the
        // resolver still resolves them via `npm run`, so `ecosystems`
        // and `tools` must surface Node too — otherwise the document is
        // internally inconsistent (tasks reference a runtime the rest of
        // the report claims absent).
        let ctx = context(vec![task("build", TaskSource::PackageJson)]);
        assert!(
            !ctx.package_managers
                .iter()
                .any(|pm| pm.ecosystem() == Ecosystem::Node),
            "precondition: no Node PM detected"
        );
        let report = DoctorReportV3::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let ecosystems = json["ecosystems"].as_array().expect("ecosystems array");
        assert!(
            ecosystems.iter().any(|e| e["name"] == "node"),
            "node ecosystem must be present when package.json tasks exist"
        );
        let tools = json["tools"].as_array().expect("tools array");
        assert!(
            tools.iter().any(|t| t["name"] == "node"),
            "node runtime tool must be probed when package.json tasks exist"
        );
    }

    #[test]
    fn v3_report_probes_detected_pms_as_tools() {
        let ctx = context(vec![]);
        let report = DoctorReportV3::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let tool = &json["tools"][0];
        assert_eq!(tool["name"], "cargo");
        assert_eq!(tool["kind"], "package-manager");
        assert_eq!(tool["id"], "tool:package-manager:cargo");
        let status = tool["probe"]["status"].as_str().expect("probe status");
        assert!(status == "found" || status == "missing");
    }
}
