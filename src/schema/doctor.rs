//! `doctor --json` schema, the structured diagnostic report.
//!
//! A structured inventory, `invocation`/`environment`/`runner`
//! provenance, per-ecosystem decisions with confidence, task `sources` as
//! first-class objects, tasks with stable `fqn`s, PATH-probe `tools`,
//! duplicate-name `conflicts`, and flattened `diagnostics`, plus a
//! self-describing `resolution` policy block.
//!
//! Notes on the shape:
//!
//! - `tasks[].resolved` and `tasks[].source` are nullable: a
//!   `package.json` script's command depends on PM resolution, which can
//!   fail, and a source anchor file can be undiscoverable.
//! - `sources[].kind` uses the structured source labels (`cargo-alias`,
//!   `just`, …) shared with `why`, not the flat `list`/`info` labels.
//! - `overrides.pm`/`overrides.runner` are bare labels; the provenance
//!   (`cli`/`env`/`config:…`) remains available on the flat `list`/`info`
//!   surface.
//! - `project.workspace` is always `null` and `project.root_source` is
//!   the root itself until workspace/root-anchor detection is modeled.
//! - Shapes nothing can emit yet are deferred rather than declared: the
//!   rich `dependency` object (`tasks[].dependencies` stays an
//!   always-empty array), `workspace`/`package_identity` objects (fields
//!   stay null), the `tool_probe_error` variant (the probe cannot
//!   error), the `binary`/`package-binary` tool kinds, and the
//!   `debug`/`error` severities. Each gets declared when an emitter
//!   exists. Contracts should describe output, not ambition.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::labels::structured_source_label;
use crate::chain::FailurePolicy;
use crate::cmd::install::InstallPlan;
use crate::cmd::run::{resolve_python_pm, select_task_entry, source_depth, source_priority};
use crate::resolver::{
    CollisionPolicy, FallbackPolicy, MismatchPolicy, ResolutionOverrides, ResolutionStep, Resolver,
    ScriptPolicy,
};
use crate::tool::node::detect_pm_from_manifest;
use crate::types::{
    DetectionWarning, Ecosystem, PackageManager, ProjectContext, Task, TaskRunner, TaskSource,
};

/// `runner doctor --json` payload.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
pub(crate) struct DoctorReport<'a> {
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
    invocation: Invocation,
    environment: Environment,
    runner: RunnerInfo,
    project: ProjectInfo,
    overrides: Overrides,
    ecosystems: Vec<EcosystemEntry>,
    sources: Vec<SourceEntry>,
    tasks: Vec<DoctorTask<'a>>,
    tools: Vec<Tool>,
    conflicts: Vec<Conflict>,
    diagnostics: Vec<Diagnostic>,
    resolution: ResolutionPolicy,
}

/// How this report came to be: the exact process invocation.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct Invocation {
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
struct Environment {
    arch: &'static str,
    os: &'static str,
    path_entries: Vec<String>,
    shell: Option<String>,
}

/// The reporting binary's own identity and contract versions.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct RunnerInfo {
    binary: String,
    name: String,
    version: &'static str,
    schema_versions: SchemaVersions,
}

/// Latest schema version each `--json` surface speaks.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct SchemaVersions {
    doctor: u32,
    list: u32,
    why: u32,
}

/// Project anchoring facts.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct ProjectInfo {
    monorepo: bool,
    root: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "What anchored root detection. Currently always the root itself (cwd or \
                           --dir); a dedicated anchor model is future work."
        )
    )]
    root_source: String,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Workspace identity. Always null today: workspace kind/root detection \
                           is not yet modeled (the monorepo flag is the coarse signal)."
        )
    )]
    workspace: Option<serde_json::Value>,
}

/// The overrides in effect for this run: `--pm`, `--fallback`, the
/// `RUNNER_*` env vars, and the `runner.toml` policy sections, reported by
/// their labels. Where each came from (CLI, env, or config) is on the
/// `list`/`info` surface instead.
// Covers every field on `ResolutionOverrides` except `parent_group_open` and
// `parent_warned`, internal runner-to-runner env markers with nothing to
// report. `every_resolution_overrides_field_is_reported_or_excluded` (bottom of
// this file) fails the build if a new field misses both this struct and that
// exclusion list.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct Overrides {
    explain: bool,
    fallback: FallbackPolicy,
    failure_policy: FailurePolicy,
    install_pms: Vec<PackageManager>,
    no_warnings: bool,
    on_collision: CollisionPolicy,
    output_grouping: OutputGrouping,
    quiet: bool,
    on_mismatch: MismatchPolicy,
    pm: Option<PackageManager>,
    pm_by_ecosystem: BTreeMap<Ecosystem, PackageManager>,
    prefer_runners: Vec<TaskRunner>,
    prefer_sources: Vec<&'static str>,
    runner: Option<TaskRunner>,
    script_policy: ScriptPolicy,
    task_source_pins: BTreeMap<String, Vec<&'static str>>,
}

/// The three grouping toggles bundled so [`Overrides`] doesn't tip
/// clippy's bool-count lint; each mirrors a same-named field on
/// [`ResolutionOverrides`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(
    feature = "schema",
    schemars(
        deny_unknown_fields,
        description = "Whether task output is grouped into collapsible blocks, under GitHub \
                       Actions and elsewhere."
    )
)]
struct OutputGrouping {
    /// Broad GitHub Actions grouping switch (`[github].group_output`).
    group_output: bool,
    /// Group parallel output under GitHub Actions
    /// (`[github].group_parallel`).
    github_group_parallel: bool,
    /// Group parallel output outside GitHub Actions
    /// (`[parallel].grouped`).
    parallel_grouped: bool,
}

/// One detected ecosystem and the PM decision made for it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct EcosystemEntry {
    decision: EcosystemDecision,
    name: &'static str,
    root: String,
    selected_package_manager: Option<&'static str>,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Detection evidence. Node carries the full signal set \
                                (lockfile/manifest/PATH probe/shim classification, keyed by \
                                tool with the shim manager as data); other ecosystems list \
                                their detected package managers.")
    )]
    signals: serde_json::Value,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct EcosystemDecision {
    confidence: Confidence,
    reason: String,
    selected: Option<&'static str>,
}

/// How sure the resolver is about an ecosystem's PM selection.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Confidence {
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
struct SourceEntry {
    exists: bool,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Stable source identity: `src:<scope>:<kind>`.")
    )]
    id: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Structured source label (same convention as `why`).")
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
            description = "Key of the container holding tasks inside the file (`scripts`, \
                           `tasks`, `alias`, …); null for flat-format files."
        )
    )]
    task_pointer: Option<&'static str>,
}

/// One task in the doctor inventory. Same identity scheme as `why`
/// (`fqn`, `source_pointer`, `aliases`, `definition`, `resolved`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct DoctorTask<'a> {
    aliases: Vec<&'a str>,
    cwd: String,
    definition: Option<&'a str>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Task dependencies. Always empty today: no extractor records dependency \
                           edges yet; the edge shape lands with the first extractor."
        )
    )]
    dependencies: Vec<serde_json::Value>,
    description: Option<&'a str>,
    fqn: String,
    #[cfg_attr(
        feature = "schema",
        schemars(description = "True when this task is an alias for another target; \
                                `definition` holds the target it expands to (e.g. cargo `b` → \
                                `build`).")
    )]
    is_alias: bool,
    name: &'a str,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "Effective command preview. Null when it depends on a PM resolution \
                           that failed."
        )
    )]
    resolved: Option<String>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "True when runner can run this task without its source's primary tool. \
                           Only deno tasks runner can execute via the embedded task shell (leaf \
                           command, no `dependencies`, no `deno` invocation) qualify today; all \
                           other sources are false."
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
enum DependencyKind {
    Runtime,
    PackageManager,
    TaskRunner,
}

impl DependencyKind {
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
struct Tool {
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Stable tool identity: `tool:<kind>:<name>`.")
    )]
    id: String,
    kind: DependencyKind,
    name: &'static str,
    probe: ToolProbe,
    required: bool,
}

/// PATH-probe outcome, tagged by `status`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
enum ToolProbe {
    Found {
        path: String,
        #[cfg_attr(
            feature = "schema",
            schemars(description = "Resolved version: taken from detection when known, \
                                    otherwise read by running `<binary> --version`. Null when \
                                    the binary reports no parseable version.")
        )]
        version: Option<String>,
    },
    Missing,
}

/// A task name claimed by more than one source: who wins, who is shadowed.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct Conflict {
    kind: &'static str,
    reason: String,
    #[cfg_attr(feature = "schema", schemars(description = "FQN of the winning task."))]
    selected: String,
    selector: String,
    severity: Severity,
    shadowed: Vec<String>,
}

/// Severity of a conflict or diagnostic. The draft's `debug`/`error`
/// levels join when something emits them.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Info,
    Warning,
}

/// One detection/resolution diagnostic, flattened from the warning
/// streams.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct Diagnostic {
    #[cfg_attr(
        feature = "schema",
        schemars(description = "Stable warning category (the warning's source subsystem).")
    )]
    code: &'static str,
    message: String,
    severity: Severity,
    source: Option<&'static str>,
    task: Option<String>,
}

/// Self-description of the task-selection policy, so consumers don't
/// hardcode runner's precedence rules.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct ResolutionPolicy {
    fqn_policy: &'static str,
    precedence: Vec<&'static str>,
    short_name_policy: &'static str,
}

impl<'a> DoctorReport<'a> {
    /// Build the report. `resolve_shims` is forwarded to the Volta shim
    /// classifier exactly like the flat `list`/`info` builder.
    pub(crate) fn build(
        ctx: &'a ProjectContext,
        overrides: &ResolutionOverrides,
        resolve_shims: bool,
    ) -> Self {
        let node_pm = Resolver::new(ctx, overrides).resolve_node_pm();
        let plan = crate::cmd::install::plan_install(ctx, overrides);

        // A collision is the install plan's verdict, not a detection fact, so
        // it joins the diagnostics here rather than riding in `ctx.warnings`
        // where every command would flush it. A plan that refuses to resolve
        // reports as a diagnostic too: `doctor` has to survive the
        // configuration it exists to explain.
        let plan_diagnostics: Vec<Diagnostic> = match &plan {
            Ok(plan) => plan
                .collisions
                .iter()
                .map(|collision| Diagnostic {
                    code: "install",
                    message: crate::cmd::install::collision_warning(
                        collision.dir,
                        &collision.writers,
                    ),
                    severity: Severity::Warning,
                    source: Some("install"),
                    task: None,
                })
                .collect(),
            Err(err) => vec![Diagnostic {
                code: "install",
                message: err.to_string(),
                severity: Severity::Warning,
                source: Some("install"),
                task: None,
            }],
        };
        let diagnostics = ctx
            .warnings
            .iter()
            .chain(node_pm.as_ref().map_or(&[][..], |d| &d.warnings))
            .map(diagnostic)
            .chain(plan_diagnostics)
            .collect();

        Self {
            schema: super::schema_url("doctor"),
            schema_version: super::SCHEMA_VERSION,
            kind: "runner.doctor",
            invocation: invocation(),
            environment: environment(),
            runner: runner_info(),
            project: ProjectInfo {
                monorepo: ctx.is_monorepo,
                root: ctx.root.display().to_string(),
                root_source: ctx.root.display().to_string(),
                workspace: None,
            },
            overrides: overrides_report(overrides),
            ecosystems: ecosystems(ctx, overrides, &node_pm, resolve_shims),
            sources: sources(ctx),
            tasks: tasks(ctx, &node_pm, overrides),
            tools: tools(ctx, overrides, &node_pm),
            conflicts: conflicts(ctx, overrides, plan.as_ref().ok()),
            diagnostics,
            resolution: ResolutionPolicy {
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

fn invocation() -> Invocation {
    Invocation {
        argv: std::env::args().collect(),
        cwd: std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
        started_at: rfc3339_utc_now(),
    }
}

fn environment() -> Environment {
    Environment {
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

fn runner_info() -> RunnerInfo {
    let binary = std::env::current_exe()
        .map_or_else(|_| "runner".to_string(), |exe| exe.display().to_string());
    let name = std::env::args_os()
        .next()
        .and_then(|arg0| crate::bin_name_from_arg0(&arg0))
        .unwrap_or_else(|| "runner".to_string());
    RunnerInfo {
        binary,
        name,
        version: env!("CARGO_PKG_VERSION"),
        schema_versions: SchemaVersions {
            doctor: super::SCHEMA_VERSION,
            list: super::SCHEMA_VERSION,
            why: super::SCHEMA_VERSION,
        },
    }
}

fn overrides_report(overrides: &ResolutionOverrides) -> Overrides {
    Overrides {
        explain: overrides.explain,
        fallback: overrides.fallback,
        failure_policy: overrides.failure_policy,
        install_pms: overrides.install_pms.clone(),
        no_warnings: overrides.no_warnings,
        on_collision: overrides.on_collision,
        output_grouping: OutputGrouping {
            group_output: overrides.group_output,
            github_group_parallel: overrides.github_group_parallel,
            parallel_grouped: overrides.parallel_grouped,
        },
        quiet: overrides.quiet,
        on_mismatch: overrides.on_mismatch,
        pm: overrides.pm.as_ref().map(|o| o.pm),
        pm_by_ecosystem: overrides
            .pm_by_ecosystem
            .iter()
            .map(|(&eco, o)| (eco, o.pm))
            .collect(),
        prefer_runners: overrides.prefer_runners.clone(),
        prefer_sources: overrides
            .prefer_sources
            .iter()
            .map(|&source| structured_source_label(source))
            .collect(),
        runner: overrides.runner.as_ref().map(|o| o.runner),
        script_policy: overrides.script_policy,
        task_source_pins: overrides
            .task_source_overrides
            .iter()
            .map(|(name, sources)| {
                (
                    name.clone(),
                    sources
                        .iter()
                        .map(|&source| structured_source_label(source))
                        .collect(),
                )
            })
            .collect(),
    }
}

fn ecosystems(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    resolve_shims: bool,
) -> Vec<EcosystemEntry> {
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
    // fallback, override). In those cases `tasks` still resolves
    // `package.json` scripts via `npm run`, so dropping Node here would
    // emit an internally inconsistent document. Same predicate gates the
    // node runtime entry in `tools`.
    if has_node_context(ctx, node_pm) && !seen.contains(&Ecosystem::Node) {
        seen.push(Ecosystem::Node);
    }
    // Same reasoning as the Node patch-in above: a `[project.scripts]`
    // task can resolve via `--pm`/`[pm].python`/detected PM without a
    // lockfile-detected PM in `package_managers`.
    if has_python_context(ctx, overrides) && !seen.contains(&Ecosystem::Python) {
        seen.push(Ecosystem::Python);
    }

    seen.into_iter()
        .map(|eco| match eco {
            Ecosystem::Node => node_ecosystem(ctx, node_pm, resolve_shims),
            Ecosystem::Python => python_ecosystem(ctx, overrides),
            other => single_pm_ecosystem(ctx, other),
        })
        .collect()
}

/// Whether the project carries Node context, considering resolver and
/// task signals, not just lockfile-detected `package_managers`. A Node
/// PM decision (`resolve_node_pm` Ok), a detected Node-ecosystem PM, or
/// any `package.json`-sourced task each count. Gates Node inclusion in
/// both [`ecosystems`] and [`tools`] so the two surfaces never
/// disagree with what `tasks` resolves.
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

/// Whether the project carries Python context, considering resolver and
/// task signals, not just lockfile-detected `package_managers`. Mirrors
/// [`has_node_context`]; gates Python inclusion in both [`ecosystems`]
/// and [`tools`] so neither surface disagrees with what `tasks`
/// resolves.
fn has_python_context(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> bool {
    resolve_python_pm(ctx, overrides).is_some()
        || ctx
            .package_managers
            .iter()
            .any(|pm| pm.ecosystem() == Ecosystem::Python)
        || ctx
            .tasks
            .iter()
            .any(|t| matches!(t.source, TaskSource::PyprojectScripts))
}

fn node_ecosystem(
    ctx: &ProjectContext,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    resolve_shims: bool,
) -> EcosystemEntry {
    let (decision, selected) = match node_pm {
        Ok(decision) => (
            EcosystemDecision {
                confidence: confidence_for_step(&decision.via),
                reason: decision.describe(),
                selected: Some(decision.pm.label()),
            },
            Some(decision.pm.label()),
        ),
        Err(err) => (
            EcosystemDecision {
                confidence: Confidence::None,
                reason: format!("{err}"),
                selected: None,
            },
            None,
        ),
    };

    let manifest_decl = detect_pm_from_manifest(&ctx.root);
    let probes = super::project::probe_signals(&ctx.root, resolve_shims);
    // Shims are keyed by tool and carry the shim *manager* as data, not
    // as the field name. Volta is merely the first manager the prober
    // classifies; asdf/mise/proto entries slot in without a contract
    // change. (The flat `list`/`info` shape's `volta_shims` spelling is
    // frozen; only this structured report gets the generic shape.)
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

    EcosystemEntry {
        decision,
        name: "node",
        root: ctx.root.display().to_string(),
        selected_package_manager: selected,
        signals,
    }
}

fn python_ecosystem(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> EcosystemEntry {
    let resolved = resolve_python_pm(ctx, overrides);
    let (decision, selected) = resolved.map_or_else(
        || {
            (
                EcosystemDecision {
                    confidence: Confidence::None,
                    reason: "no Python package manager detected".to_string(),
                    selected: None,
                },
                None,
            )
        },
        |decision| {
            let label = decision.pm.label();
            (
                EcosystemDecision {
                    confidence: Confidence::High,
                    reason: decision.describe(),
                    selected: Some(label),
                },
                Some(label),
            )
        },
    );

    EcosystemEntry {
        decision,
        name: "python",
        root: ctx.root.display().to_string(),
        selected_package_manager: selected,
        signals: detected_pm_signals(ctx, Ecosystem::Python),
    }
}

/// Single-PM ecosystems (rust/go/deno/ruby/php): the detected manager
/// *is* the decision; there is no competing-PM resolution chain.
fn single_pm_ecosystem(ctx: &ProjectContext, eco: Ecosystem) -> EcosystemEntry {
    let selected = ctx
        .package_managers
        .iter()
        .find(|pm| pm.ecosystem() == eco)
        .map(|pm| pm.label());

    EcosystemEntry {
        decision: EcosystemDecision {
            confidence: Confidence::High,
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

const fn confidence_for_step(step: &ResolutionStep) -> Confidence {
    match step {
        ResolutionStep::Override(_)
        | ResolutionStep::ManifestPackageManager
        | ResolutionStep::ManifestDevEngines { .. }
        | ResolutionStep::Lockfile => Confidence::High,
        ResolutionStep::PathProbe { .. } => Confidence::Medium,
        ResolutionStep::LegacyNpmFallback => Confidence::Low,
    }
}

fn sources(ctx: &ProjectContext) -> Vec<SourceEntry> {
    let mut seen: Vec<TaskSource> = Vec::new();
    for task in &ctx.tasks {
        if !seen.contains(&task.source) {
            seen.push(task.source);
        }
    }

    seen.into_iter()
        .map(|source| {
            let kind = structured_source_label(source);
            let anchor = super::labels::source_anchor(source, &ctx.root);
            let path = anchor
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string());
            let relpath = anchor.as_ref().map_or_else(String::new, |p| {
                p.strip_prefix(&ctx.root).unwrap_or(p).display().to_string()
            });
            SourceEntry {
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

fn tasks<'a>(
    ctx: &'a ProjectContext,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    overrides: &ResolutionOverrides,
) -> Vec<DoctorTask<'a>> {
    let node_pm_label = node_pm.as_ref().ok().map(|d| d.pm.label());
    let python_pm_label = resolve_python_pm(ctx, overrides).map(|d| d.pm.label());

    // `anchor_file` walks the filesystem; resolve each distinct source
    // once instead of once per task.
    let mut anchors: std::collections::HashMap<TaskSource, Option<String>> =
        std::collections::HashMap::new();
    for task in &ctx.tasks {
        anchors.entry(task.source).or_insert_with(|| {
            super::labels::source_anchor(task.source, &ctx.root).map(|p| p.display().to_string())
        });
    }

    ctx.tasks
        .iter()
        .map(|task| DoctorTask {
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
            fqn: super::labels::fqn(task.source, &task.name),
            is_alias: task.alias_of.is_some(),
            name: &task.name,
            resolved: super::labels::resolved_command(task, node_pm_label, python_pm_label),
            self_executable: deno_task_self_executable(ctx, task),
            source: anchors.get(&task.source).cloned().flatten(),
            source_pointer: super::labels::source_pointer(task),
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

fn tools(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    node_pm: &Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
) -> Vec<Tool> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var_os("PATHEXT");
    let pathext_ref = pathext.as_deref();

    let mut tools = Vec::new();

    if has_node_context(ctx, node_pm) {
        tools.push(probe_tool(
            "node",
            DependencyKind::Runtime,
            ctx.current_node
                .as_deref()
                .map(|v| v.trim_start_matches('v').to_string()),
            true,
            &path,
            pathext_ref,
        ));
    }
    // Same reasoning as the node runtime probe above: a resolved
    // `uv run <task>` must never reference an interpreter the tools
    // surface claims absent.
    if has_python_context(ctx, overrides) {
        use crate::tool::python::PYTHON_BIN;

        tools.push(probe_tool(
            PYTHON_BIN,
            DependencyKind::Runtime,
            None,
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
            DependencyKind::PackageManager,
            None,
            required,
            &path,
            pathext_ref,
        ));
    }
    for runner in &ctx.task_runners {
        tools.push(probe_tool(
            runner.label(),
            DependencyKind::TaskRunner,
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
    kind: DependencyKind,
    version: Option<String>,
    required: bool,
    path: &std::ffi::OsStr,
    pathext: Option<&std::ffi::OsStr>,
) -> Tool {
    let probe = crate::resolver::probe_path_for_doctor(name, path, pathext).map_or(
        ToolProbe::Missing,
        |hit| ToolProbe::Found {
            // Prefer a version already known from detection (the node
            // runtime); otherwise ask the binary directly.
            version: version.or_else(|| probe_tool_version(&hit)),
            path: hit.display().to_string(),
        },
    );
    Tool {
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

fn conflicts(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    plan: Option<&InstallPlan>,
) -> Vec<Conflict> {
    let mut by_name: BTreeMap<&str, Vec<&Task>> = BTreeMap::new();
    for task in &ctx.tasks {
        by_name.entry(&task.name).or_default().push(task);
    }

    let duplicate_names = by_name
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .map(|(name, group)| {
            let selected = select_task_entry(ctx, overrides, &group);
            let fqn_of = |task: &Task| super::labels::fqn(task.source, &task.name);
            Conflict {
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
                severity: Severity::Info,
                shadowed: group
                    .iter()
                    .filter(|task| !std::ptr::eq(**task, selected))
                    .map(|task| fqn_of(task))
                    .collect(),
            }
        });

    duplicate_names
        .chain(plan.into_iter().flat_map(install_dir_conflicts))
        .collect()
}

/// The install plan's directory verdicts, in the same who-wins/who-is-shadowed
/// shape a duplicate task name reports under.
///
/// Only *resolved* directories appear. A directory the user told runner to
/// share has no winner and no shadowed party (every writer runs), so it
/// reports as a diagnostic instead of a conflict with two lying fields.
fn install_dir_conflicts(plan: &InstallPlan) -> Vec<Conflict> {
    plan.shadowed
        .iter()
        .map(|shadow| Conflict {
            kind: "install-dir-collision",
            reason: format!(
                "{} and {} both install into {}/; the package manager resolved for the ecosystem \
                 installs it and the other is skipped. List both in `[install].pms` to run them \
                 anyway.",
                shadow.winner.label(),
                shadow.loser.label(),
                shadow.dir,
            ),
            selected: shadow.winner.label().to_string(),
            selector: shadow.dir.to_string(),
            severity: Severity::Info,
            shadowed: vec![shadow.loser.label().to_string()],
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

fn diagnostic(warning: &DetectionWarning) -> Diagnostic {
    Diagnostic {
        code: warning.source(),
        message: warning.detail(),
        severity: Severity::Warning,
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

    use super::{DoctorReport, rfc3339_utc};
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
            install_dirs: Vec::new(),
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
        // 2000-02-29, leap day in a century-leap year.
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(951_868_799), "2000-02-29T23:59:59Z");
        assert_eq!(rfc3339_utc(951_868_800), "2000-03-01T00:00:00Z");
    }

    #[test]
    fn v3_report_carries_contract_constants() {
        let ctx = context(vec![]);
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        assert_eq!(json["kind"], "runner.doctor");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["overrides"]["quiet"], serde_json::json!(false));
        assert!(
            json["$schema"]
                .as_str()
                .is_some_and(|s| s.contains("doctor.schema.json"))
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
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
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
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
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
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
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
        // and `tools` must surface Node too; otherwise the document is
        // internally inconsistent (tasks reference a runtime the rest of
        // the report claims absent).
        let ctx = context(vec![task("build", TaskSource::PackageJson)]);
        assert!(
            !ctx.package_managers
                .iter()
                .any(|pm| pm.ecosystem() == Ecosystem::Node),
            "precondition: no Node PM detected"
        );
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
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
    fn v3_report_keeps_python_when_only_pyproject_scripts_tasks_present() {
        // Mirrors v3_report_keeps_node_when_only_package_json_tasks_present:
        // a bare pyproject.toml with [project.scripts] but no uv.lock/poetry
        // markers still resolves tasks via the detected/overridden Python
        // PM, so ecosystems/tools must surface Python too.
        use crate::tool::python::PYTHON_BIN;

        let ctx = context(vec![task("build", TaskSource::PyprojectScripts)]);
        assert!(
            !ctx.package_managers
                .iter()
                .any(|pm| pm.ecosystem() == Ecosystem::Python),
            "precondition: no Python PM detected"
        );
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let ecosystems = json["ecosystems"].as_array().expect("ecosystems array");
        assert!(
            ecosystems.iter().any(|e| e["name"] == "python"),
            "python ecosystem must be present when pyproject.toml tasks exist"
        );
        let tools = json["tools"].as_array().expect("tools array");
        assert!(
            tools.iter().any(|t| t["name"] == PYTHON_BIN),
            "python runtime tool must be probed when pyproject.toml tasks exist"
        );
    }

    #[test]
    fn v3_report_probes_detected_pms_as_tools() {
        let ctx = context(vec![]);
        let report = DoctorReport::build(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let tool = &json["tools"][0];
        assert_eq!(tool["name"], "cargo");
        assert_eq!(tool["kind"], "package-manager");
        assert_eq!(tool["id"], "tool:package-manager:cargo");
        let status = tool["probe"]["status"].as_str().expect("probe status");
        assert!(status == "found" || status == "missing");
    }

    #[test]
    fn report_surfaces_previously_missing_override_fields() {
        use std::collections::BTreeMap;

        use crate::chain::FailurePolicy;
        use crate::resolver::ScriptPolicy;
        use crate::types::PackageManager;

        let overrides = ResolutionOverrides {
            failure_policy: FailurePolicy::KeepGoing,
            group_output: false,
            github_group_parallel: false,
            parallel_grouped: true,
            install_pms: vec![PackageManager::Npm, PackageManager::Pnpm],
            script_policy: ScriptPolicy::Deny,
            prefer_sources: vec![TaskSource::Justfile, TaskSource::CargoAliases],
            task_source_overrides: BTreeMap::from([(
                "build".to_string(),
                vec![TaskSource::Justfile],
            )]),
            ..ResolutionOverrides::default()
        };

        let ctx = context(vec![]);
        let report = DoctorReport::build(&ctx, &overrides, false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let ov = &json["overrides"];
        assert_eq!(ov["failure_policy"], "keep-going");
        assert_eq!(ov["output_grouping"]["group_output"], false);
        assert_eq!(ov["output_grouping"]["github_group_parallel"], false);
        assert_eq!(ov["output_grouping"]["parallel_grouped"], true);
        assert_eq!(ov["install_pms"], serde_json::json!(["npm", "pnpm"]));
        assert_eq!(ov["script_policy"], "deny");
        assert_eq!(
            ov["prefer_sources"],
            serde_json::json!(["just", "cargo-alias"])
        );
        assert_eq!(ov["task_source_pins"]["build"], serde_json::json!(["just"]));
    }

    /// Drift guard: every field on [`ResolutionOverrides`] must appear
    /// either in the reflected [`Overrides`] schema or in the exclusion
    /// list below (with a reason). The macro's single field list both
    /// exhaustively destructures the struct (a new field fails to compile
    /// until listed) and feeds the checked names, so the list can't go
    /// stale relative to the destructure.
    ///
    /// Two fields are reported under a different name/shape than
    /// `ResolutionOverrides` uses, both to dodge clippy lints:
    /// `task_source_overrides` reports as `task_source_pins`
    /// (`struct_field_names`, it would otherwise end with the struct's
    /// own name), and `group_output`/`github_group_parallel`/
    /// `parallel_grouped` nest under `output_grouping`
    /// (`struct_excessive_bools`). `RENAMED`/the `output_grouping` unnest
    /// below account for both.
    #[cfg(feature = "schema")]
    #[test]
    fn every_resolution_overrides_field_is_reported_or_excluded() {
        // Internal runner-to-runner plumbing (inherited env markers),
        // never user overrides, nothing meaningful to report.
        const EXCLUDED: &[&str] = &["parent_group_open", "parent_warned"];
        // resolver field name -> name it's actually reported under.
        const RENAMED: &[(&str, &str)] = &[("task_source_overrides", "task_source_pins")];

        // One list, two jobs: exhaustively destructure ResolutionOverrides
        // (a new field fails to compile until added here) and name the
        // fields the assertion loop checks.
        macro_rules! resolution_overrides_fields {
            ($($field:ident),* $(,)?) => {{
                let ResolutionOverrides { $($field: _),* } = ResolutionOverrides::default();
                [$(stringify!($field)),*]
            }};
        }
        let resolution_overrides_fields = resolution_overrides_fields![
            pm,
            pm_by_ecosystem,
            runner,
            prefer_runners,
            prefer_sources,
            task_source_overrides,
            fallback,
            on_mismatch,
            no_warnings,
            quiet,
            explain,
            failure_policy,
            group_output,
            github_group_parallel,
            parallel_grouped,
            install_pms,
            script_policy,
            on_collision,
            parent_group_open,
            parent_warned,
        ];

        let schema = serde_json::to_value(schemars::schema_for!(super::Overrides))
            .expect("Overrides schema should serialize");
        let top_properties = schema["properties"]
            .as_object()
            .expect("Overrides schema must have properties");
        let mut reported: std::collections::BTreeSet<&str> =
            top_properties.keys().map(String::as_str).collect();

        // Unnest OutputGrouping so its 3 fields match by their
        // ResolutionOverrides names instead of living behind a
        // container the resolver struct doesn't have.
        reported.remove("output_grouping");
        let grouping_def = top_properties["output_grouping"]["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix("#/$defs/"))
            .expect("output_grouping field must $ref a $defs entry");
        let grouping_properties = schema["$defs"][grouping_def]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{grouping_def}: expected a properties object"));
        reported.extend(grouping_properties.keys().map(String::as_str));

        for field in resolution_overrides_fields {
            if EXCLUDED.contains(&field) {
                assert!(
                    !reported.contains(field),
                    "{field}: excluded field must not also appear in Overrides"
                );
                continue;
            }
            let reported_name = RENAMED
                .iter()
                .find_map(|&(from, to)| (from == field).then_some(to))
                .unwrap_or(field);
            assert!(
                reported.contains(reported_name),
                "{field}: ResolutionOverrides field is neither reported by Overrides (as \
                 {reported_name:?}) nor on the EXCLUDED allowlist, add it to one"
            );
        }
    }

    /// The closed key set depends on `Ecosystem` variants carrying no doc
    /// comments (see `src/types.rs`); a `///` there silently reverts the
    /// map to open `additionalProperties`. This pins the shape.
    #[cfg(feature = "schema")]
    #[test]
    fn pm_by_ecosystem_schema_keys_stay_closed() {
        let schema = serde_json::to_value(schemars::schema_for!(super::Overrides))
            .expect("Overrides schema should serialize");
        let map_schema = &schema["properties"]["pm_by_ecosystem"];

        assert_eq!(
            map_schema["additionalProperties"],
            serde_json::json!(false),
            "pm_by_ecosystem must reject unknown keys"
        );
        let keys: Vec<&str> = map_schema["properties"]
            .as_object()
            .expect("pm_by_ecosystem must enumerate its keys")
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected: Vec<&str> = Ecosystem::ALL.iter().map(|eco| eco.label()).collect();
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }
}
