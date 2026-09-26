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
//! - `project.workspace` lists the root's workspace declarations and
//!   members; `project.root_source` is the root itself until root-anchor
//!   detection is modeled.
//! - Shapes nothing can emit yet are deferred rather than declared: the
//!   rich `dependency` object (`tasks[].dependencies` stays an
//!   always-empty array), the `tool_probe_error` variant (the probe cannot
//!   error), the `binary`/`package-binary` tool kinds, and the
//!   `debug`/`error` severities. Each gets declared when an emitter
//!   exists. Contracts should describe output, not ambition.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::labels::{StructuredSource, structured_source_label};
use crate::chain::FailurePolicy;
use crate::commands::install::InstallPlan;
use crate::commands::run::decision::{Observed, PmDecision};
use crate::resolver::{
    CollisionPolicy, FallbackPolicy, LockfilePolicy, MismatchPolicy, OutputGrouping,
    ResolutionOverrides, ScriptPolicy,
};
use crate::types::{
    DetectionWarning, Ecosystem, JsRuntime, PackageManager, ProjectContext, Task, TaskRunner,
    TaskSource,
};

/// `runner doctor --json` payload.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(
    deny_unknown_fields,
    title = "runner doctor --json",
    description = "JSON schema for `runner doctor --json`: structured diagnostic inventory with \
                   invocation/environment provenance, per-ecosystem decisions, sources, \
                   fqn-keyed tasks, tools, conflicts, and diagnostics.",
    extend("$id" = super::schema_url("doctor"))
)]
pub(crate) struct DoctorReport<'a> {
    #[serde(rename = "$schema")]
    #[schemars(description = "URI of the JSON Schema that describes this payload.")]
    schema: String,
    #[schemars(
        description = "Schema contract version for this JSON payload.",
        extend("const" = super::SCHEMA_VERSION)
    )]
    schema_version: u32,
    #[schemars(description = "Payload discriminator; always \"runner.doctor\".")]
    kind: &'static str,
    invocation: Invocation,
    environment: Environment,
    runner: RunnerInfo,
    project: ProjectInfo<'a>,
    overrides: Overrides,
    ecosystems: Vec<EcosystemEntry>,
    sources: Vec<SourceEntry<'a>>,
    tasks: Vec<DoctorTask<'a>>,
    tools: Vec<Tool>,
    conflicts: Vec<Conflict>,
    diagnostics: Vec<Diagnostic>,
    resolution: ResolutionPolicy,
}

/// The variable *names* each env layer sets. Values never appear here.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct EnvNames {
    project: Vec<String>,
    tool: BTreeMap<String, Vec<String>>,
    task: BTreeMap<String, Vec<String>>,
}

/// How this report came to be: the exact process invocation.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct Invocation {
    argv: Vec<String>,
    cwd: String,
    #[schemars(description = "UTC RFC 3339 timestamp of report generation.")]
    started_at: String,
}

/// Host facts that influence probing and dispatch.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct Environment {
    arch: &'static str,
    os: &'static str,
    path_entries: Vec<String>,
    shell: Option<String>,
}

/// The reporting binary's own identity and contract versions.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct RunnerInfo {
    binary: String,
    name: String,
    version: &'static str,
    schema_versions: SchemaVersions,
}

/// Latest schema version each `--json` surface speaks.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct SchemaVersions {
    doctor: u32,
    list: u32,
    why: u32,
}

/// Project anchoring facts.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct ProjectInfo<'a> {
    monorepo: bool,
    root: String,
    #[schemars(
        description = "What anchored root detection: the root itself (cwd or --dir), or \
                       `workspace root of member <name>` when the invocation directory sits \
                       inside a workspace member."
    )]
    root_source: String,
    #[schemars(
        description = "Workspace declarations at the root and their members; null when the root \
                       declares no workspace."
    )]
    workspace: Option<super::project::WorkspaceInfo<'a>>,
}

/// The overrides in effect for this run: `--pm`, `--fallback`, the
/// `RUNNER_*` env vars, and the `runner.toml` policy sections, reported by
/// their labels. Where each came from (CLI, env, or config) is on the
/// `list`/`info` surface instead.
// Covers every field on `ResolutionOverrides` except `parent`, internal
// runner-to-runner env markers with nothing to report.
// `every_resolution_overrides_field_is_reported_or_excluded` (bottom of
// this file) fails the build if a new field misses both this struct and that
// exclusion list.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct Overrides {
    explain: bool,
    fallback: FallbackPolicy,
    failure_policy: FailurePolicy,
    /// The install allowlist. Install has none, so this is always empty.
    install_pms: Vec<PackageManager>,
    no_warnings: bool,
    on_collision: CollisionPolicy,
    output_grouping: OutputGrouping,
    quiet: bool,
    output: OutputPolicyReport,
    on_mismatch: MismatchPolicy,
    pm: Option<PackageManager>,
    pm_by_ecosystem: BTreeMap<Ecosystem, PackageManager>,
    prefer_runners: Vec<TaskRunner>,
    prefer_sources: Vec<StructuredSource>,
    runner: Option<TaskRunner>,
    runtime: Option<JsRuntime>,
    script_policy: ScriptPolicy,
    #[schemars(extend("enum" = ["ask", "allow", "local"]))]
    fetch: &'static str,
    lockfile: LockfilePolicy,
    #[schemars(
        description = "Variable names each `env` layer sets, narrowest last. Values are withheld: \
                       this payload is meant to be pasted into a bug report."
    )]
    env: EnvNames,
    #[schemars(
        description = "`[tools.<name>].install`, the operations `runner install` runs for each \
                       tool, in order."
    )]
    tool_install: BTreeMap<String, Vec<String>>,
    task_source_pins: BTreeMap<String, Vec<StructuredSource>>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct OutputPolicyReport {
    #[schemars(extend("enum" = ["off", "quiet", "very-quiet", "silent", "mute"]))]
    level: &'static str,
    #[serde(flatten)]
    runner: crate::tool::RunnerOutputPolicy,
    #[schemars(extend("enum" = ["normal", "quiet", "reduced"]))]
    host_diagnostics: &'static str,
    #[schemars(extend("enum" = ["inherit", "stderr"]))]
    host_stream: &'static str,
}

/// One detected ecosystem and the PM decision made for it.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct EcosystemEntry {
    decision: EcosystemDecision,
    name: &'static str,
    root: String,
    selected_package_manager: Option<&'static str>,
    #[schemars(description = "Detection evidence. Node carries the full signal set \
                              (lockfile/manifest/PATH probe/shim classification, keyed by tool \
                              with the shim manager as data); other ecosystems list their \
                              detected package managers.")]
    signals: serde_json::Value,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct EcosystemDecision {
    confidence: Confidence,
    reason: String,
    selected: Option<&'static str>,
}

/// How sure the resolver is about an ecosystem's PM selection.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Confidence {
    /// Explicit signal: override, manifest declaration, or lockfile.
    High,
    /// Inferred: PATH probe found a usable binary.
    Medium,
    /// Resolution failed.
    None,
}

/// One task-source config file as a first-class object.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct SourceEntry<'a> {
    exists: bool,
    #[schemars(description = "Stable source identity: `src:<scope>:<kind>`.")]
    id: String,
    #[schemars(description = "Structured source label (same convention as `why`).")]
    kind: StructuredSource,
    #[schemars(
        description = "Workspace member identity (`name`, `path`) for member sources; null for \
                       root sources."
    )]
    package: Option<serde_json::Value>,
    path: String,
    relpath: String,
    #[schemars(description = "`root`, or the workspace member name the source belongs to.")]
    scope: &'a str,
    #[schemars(
        description = "Key of the container holding tasks inside the file (`scripts`, `tasks`, \
                       `alias`, …); null for flat-format files."
    )]
    task_pointer: Option<&'static str>,
}

/// One task in the doctor inventory. Same identity scheme as `why`
/// (`fqn`, `source_pointer`, `aliases`, `definition`, `resolved`).
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct DoctorTask<'a> {
    aliases: Vec<&'a str>,
    cwd: String,
    definition: Option<&'a str>,
    #[schemars(
        description = "Tasks that run before this one, as the source declares them. Filled from \
                       `mise tasks --json`; empty for sources without dependency edges."
    )]
    dependencies: Vec<&'a str>,
    description: Option<&'a str>,
    fqn: String,
    #[schemars(
        description = "True when this task is an alias for another target; `definition` holds the \
                       target it expands to (e.g. cargo `b` → `build`)."
    )]
    is_alias: bool,
    name: &'a str,
    #[schemars(
        description = "Effective command preview. Null when it depends on a PM resolution that \
                       failed."
    )]
    resolved: Option<String>,
    #[schemars(description = "`root`, or the workspace member name the task belongs to.")]
    scope: &'a str,
    #[schemars(
        description = "True when runner can run this task without its source's primary tool. Only \
                       deno tasks runner can execute via the embedded task shell (leaf command, \
                       no `dependencies`, no `deno` invocation) qualify today; all other sources \
                       are false."
    )]
    self_executable: bool,
    source: Option<String>,
    source_pointer: Option<String>,
}

/// What kind of thing a probed tool is. The draft's `binary` /
/// `package-binary` kinds join when something probes them.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize)]
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
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
struct Tool {
    #[schemars(description = "Stable tool identity: `tool:<kind>:<name>`.")]
    id: String,
    kind: DependencyKind,
    name: &'static str,
    probe: ToolProbe,
    required: bool,
}

/// PATH-probe outcome, tagged by `status`.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
#[schemars(deny_unknown_fields)]
enum ToolProbe {
    Found {
        path: String,
        #[schemars(
            description = "Resolved version: taken from detection when known, otherwise read by \
                           running `<binary> --version`. Null when the binary reports no \
                           parseable version."
        )]
        version: Option<String>,
    },
    Missing,
}

/// A task-name or install-directory conflict, tagged by `kind`.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[serde(tag = "kind")]
#[schemars(deny_unknown_fields)]
enum Conflict {
    /// A task name claimed by more than one source: which task wins and which
    /// fully-qualified task names are shadowed.
    #[serde(rename = "duplicate-task-name")]
    DuplicateTaskName {
        reason: String,
        #[schemars(description = "FQN of the winning task.")]
        selected: String,
        #[schemars(description = "Conflicting task name.")]
        selector: String,
        severity: Severity,
        #[schemars(description = "FQNs of the shadowed tasks.")]
        shadowed: Vec<String>,
    },
    /// Package managers that write the same installation directory: which
    /// package manager installs it and which package managers are shadowed.
    #[serde(rename = "install-dir-collision")]
    InstallDirCollision {
        reason: String,
        #[schemars(description = "Label of the selected package manager.")]
        selected: String,
        #[schemars(description = "Path of the conflicting installation directory.")]
        selector: String,
        severity: Severity,
        #[schemars(description = "Labels of the shadowed package managers.")]
        shadowed: Vec<String>,
    },
}

/// Severity of a conflict or diagnostic. The draft's `debug`/`error`
/// levels join when something emits them.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Info,
    Warning,
}

/// One detection/resolution diagnostic, flattened from the warning
/// streams.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
pub(crate) struct Diagnostic {
    #[schemars(description = "Stable warning category (the warning's source subsystem).")]
    code: &'static str,
    pub(crate) message: String,
    severity: Severity,
    pub(crate) source: Option<&'static str>,
    task: Option<String>,
}

/// Self-description of the task-selection policy, so consumers don't
/// hardcode runner's precedence rules.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(deny_unknown_fields)]
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
        let observed = Observed::observe(ctx, overrides);
        let decisions = Decisions::from_observed(&observed, overrides);
        let plan = crate::commands::install::plan_install(ctx, overrides);

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
                    message: crate::commands::install::collision_warning(
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
            .chain(decisions.warnings.iter())
            .map(diagnostic)
            .chain(plan_diagnostics)
            .chain(provider_diagnostics(ctx, overrides))
            .collect();

        Self {
            schema: super::schema_url("doctor"),
            schema_version: super::SCHEMA_VERSION,
            kind: "runner.doctor",
            invocation: invocation(),
            environment: environment(),
            runner: runner_info(),
            project: ProjectInfo {
                monorepo: ctx.is_monorepo(),
                root: ctx.root.display().to_string(),
                root_source: ctx.current_member().map_or_else(
                    || ctx.root.display().to_string(),
                    |member| format!("workspace root of member {}", member.name),
                ),
                workspace: ctx
                    .workspace
                    .as_ref()
                    .map(super::project::WorkspaceInfo::from_workspace),
            },
            overrides: overrides_report(overrides),
            ecosystems: ecosystems(ctx, &decisions, resolve_shims),
            sources: sources(ctx),
            tasks: tasks(ctx, overrides),
            tools: tools(ctx, &decisions),
            conflicts: conflicts(ctx, observed.as_ref().ok(), plan.as_ref().ok()),
            diagnostics,
            resolution: resolution_policy(),
        }
    }
}

const EXAMPLE_ROOT: &str = "/path/to/project";

impl DoctorReport<'static> {
    /// The fixed report committed as `schemas/doctor.example.json`.
    pub(crate) fn example() -> Self {
        Self {
            schema: super::schema_url("doctor"),
            schema_version: super::SCHEMA_VERSION,
            kind: "runner.doctor",
            invocation: Invocation {
                argv: ["runner", "doctor", "--json"].map(String::from).to_vec(),
                cwd: EXAMPLE_ROOT.to_string(),
                started_at: "2026-01-01T00:00:00Z".to_string(),
            },
            environment: Environment {
                arch: "x86_64",
                os: "linux",
                path_entries: vec!["/usr/local/bin".to_string(), "/usr/bin".to_string()],
                shell: Some("bash".to_string()),
            },
            runner: RunnerInfo {
                binary: "/usr/local/bin/runner".to_string(),
                name: "runner".to_string(),
                version: "0.0.0",
                schema_versions: SchemaVersions {
                    doctor: super::SCHEMA_VERSION,
                    list: super::SCHEMA_VERSION,
                    why: super::SCHEMA_VERSION,
                },
            },
            project: ProjectInfo {
                monorepo: false,
                root: EXAMPLE_ROOT.to_string(),
                root_source: EXAMPLE_ROOT.to_string(),
                workspace: None,
            },
            overrides: overrides_report(
                &ResolutionOverrides::from_sources(&crate::resolver::OverrideSources::default())
                    .expect("overrides without sources build"),
            ),
            ecosystems: vec![example_node_ecosystem()],
            sources: vec![
                example_source(TaskSource::PackageJson, "package.json", Some("scripts")),
                example_source(TaskSource::Justfile, "justfile", None),
            ],
            tasks: vec![
                example_task(
                    TaskSource::PackageJson,
                    "package.json",
                    "build",
                    "bun run build",
                ),
                example_task(
                    TaskSource::PackageJson,
                    "package.json",
                    "fmt:update",
                    "bun run fmt:update",
                ),
                example_task(TaskSource::Justfile, "justfile", "build", "just build"),
            ],
            tools: vec![
                example_tool(DependencyKind::Runtime, "node", "24.0.0"),
                example_tool(DependencyKind::PackageManager, "bun", "1.1.0"),
                example_tool(DependencyKind::TaskRunner, "just", "1.36.0"),
            ],
            conflicts: vec![Conflict::DuplicateTaskName {
                reason: "2 sources define `build`; package.json runs because its source has the \
                         higher task priority"
                    .to_string(),
                selected: "root:package.json#build".to_string(),
                selector: "build".to_string(),
                severity: Severity::Info,
                shadowed: vec!["root:just#build".to_string()],
            }],
            diagnostics: Vec::new(),
            resolution: resolution_policy(),
        }
    }
}

fn example_node_ecosystem() -> EcosystemEntry {
    EcosystemEntry {
        decision: EcosystemDecision {
            confidence: Confidence::High,
            reason: "bun via package.json \"packageManager\"".to_string(),
            selected: Some("bun"),
        },
        name: "node",
        root: EXAMPLE_ROOT.to_string(),
        selected_package_manager: Some("bun"),
        signals: serde_json::json!({
            "lockfile_pm": "bun",
            "manifest_pm": "bun",
            "path_probe": { "bun": "/usr/bin/bun", "npm": "/usr/bin/npm" },
        }),
    }
}

fn example_source(
    source: TaskSource,
    relpath: &str,
    task_pointer: Option<&'static str>,
) -> SourceEntry<'static> {
    SourceEntry {
        exists: true,
        id: format!("src:root:{}", structured_source_label(source)),
        kind: StructuredSource(source),
        package: None,
        path: format!("{EXAMPLE_ROOT}/{relpath}"),
        relpath: relpath.to_string(),
        scope: "root",
        task_pointer,
    }
}

fn example_task(
    source: TaskSource,
    relpath: &str,
    name: &'static str,
    resolved: &str,
) -> DoctorTask<'static> {
    DoctorTask {
        aliases: Vec::new(),
        cwd: EXAMPLE_ROOT.to_string(),
        definition: None,
        dependencies: Vec::new(),
        description: None,
        fqn: super::labels::fqn_of("root", source, name),
        is_alias: false,
        name,
        resolved: Some(resolved.to_string()),
        scope: "root",
        self_executable: false,
        source: Some(format!("{EXAMPLE_ROOT}/{relpath}")),
        source_pointer: Some(match source {
            TaskSource::PackageJson => format!("scripts.{name}"),
            TaskSource::CargoAliases => format!("alias.{name}"),
            _ => name.to_string(),
        }),
    }
}

fn example_tool(kind: DependencyKind, name: &'static str, version: &str) -> Tool {
    Tool {
        id: format!("tool:{}:{name}", kind.label()),
        kind,
        name,
        probe: ToolProbe::Found {
            path: format!("/usr/bin/{name}"),
            version: Some(version.to_string()),
        },
        required: true,
    }
}

fn resolution_policy() -> ResolutionPolicy {
    ResolutionPolicy {
        fqn_policy: "exact-only",
        precedence: vec![
            "nearest-scope",
            "task-pin",
            "chosen-runner",
            "prefer-list",
            "chosen-dispatcher",
            "dispatch-order",
            "task-priority",
            "provider",
            "alias-last",
        ],
        short_name_policy: "deterministic-precedence",
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

/// Variable names per scope, values dropped.
fn env_names(layers: &BTreeMap<String, BTreeMap<String, String>>) -> BTreeMap<String, Vec<String>> {
    layers
        .iter()
        .map(|(scope, vars)| (scope.clone(), vars.keys().cloned().collect::<Vec<_>>()))
        .collect()
}

fn overrides_report(overrides: &ResolutionOverrides) -> Overrides {
    Overrides {
        explain: overrides.explain,
        fallback: overrides.fallback,
        failure_policy: overrides.failure_policy,
        install_pms: Vec::new(),
        no_warnings: overrides.no_warnings,
        on_collision: overrides.on_collision,
        output_grouping: overrides.grouping,
        quiet: overrides.quiet_level != crate::tool::QuietLevel::Off,
        output: OutputPolicyReport {
            level: overrides.quiet_level.label(),
            runner: overrides.runner_output_for(None),
            host_diagnostics: overrides.output_policy.host_diagnostics.label(),
            host_stream: overrides.global_host_stream().label(),
        },
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
            .copied()
            .map(StructuredSource)
            .collect(),
        runner: overrides.runner.as_ref().map(|o| o.runner),
        runtime: overrides.runtime.as_ref().map(|o| o.runtime),
        script_policy: overrides.script_policy,
        fetch: overrides.reach.label(),
        lockfile: overrides.lockfile,
        env: EnvNames {
            project: overrides.env.project.keys().cloned().collect(),
            tool: env_names(&overrides.env.tool),
            task: env_names(&overrides.env.task),
        },
        tool_install: overrides.tool_install.clone(),
        task_source_pins: overrides
            .task_source_overrides
            .iter()
            .map(|(name, sources)| {
                (
                    name.clone(),
                    sources.iter().copied().map(StructuredSource).collect(),
                )
            })
            .collect(),
    }
}

/// The package-manager decisions the report describes, per dispatched source.
pub(crate) struct Decisions {
    node: Option<PmDecision>,
    python: Option<PmDecision>,
    manifest: Option<crate::commands::run::decision::ManifestDeclaration>,
    error: Option<String>,
    warnings: Vec<DetectionWarning>,
}

impl Decisions {
    pub(crate) fn from_observed(
        observed: &std::io::Result<Observed>,
        overrides: &ResolutionOverrides,
    ) -> Self {
        let Ok(observed) = observed else {
            return Self {
                node: None,
                python: None,
                manifest: None,
                error: observed.as_ref().err().map(ToString::to_string),
                warnings: Vec::new(),
            };
        };
        let node = observed.decision(runner_core::ProviderId::PackageJson);
        let python = observed.decision(runner_core::ProviderId::Pyproject);
        let warnings = [&node, &python]
            .into_iter()
            .flatten()
            .flat_map(|decision| decision.warnings(&observed.project, overrides))
            .collect();
        Self {
            manifest: observed.manifest_declaration(runner_core::ProviderId::PackageJson),
            node,
            python,
            error: None,
            warnings,
        }
    }

    /// The decision for `ecosystem`'s scripts, when the ecosystem has one.
    const fn for_ecosystem(&self, ecosystem: Ecosystem) -> Option<&PmDecision> {
        match ecosystem {
            Ecosystem::Node => self.node.as_ref(),
            Ecosystem::Python => self.python.as_ref(),
            _ => None,
        }
    }

    /// Whether the project carries Node context: a dispatching package
    /// manager, a detected Node package manager, or a `package.json` task.
    pub(crate) fn has_node_context(&self, ctx: &ProjectContext) -> bool {
        self.node.is_some()
            || ctx
                .package_managers()
                .iter()
                .any(|pm| pm.ecosystem() == Ecosystem::Node)
            || ctx
                .tasks
                .iter()
                .any(|t| matches!(t.source, TaskSource::PackageJson))
    }

    fn has_python_context(&self, ctx: &ProjectContext) -> bool {
        self.python.is_some()
            || ctx
                .package_managers()
                .iter()
                .any(|pm| pm.ecosystem() == Ecosystem::Python)
            || ctx
                .tasks
                .iter()
                .any(|t| matches!(t.source, TaskSource::PyprojectScripts))
    }
}

fn ecosystems(
    ctx: &ProjectContext,
    decisions: &Decisions,
    resolve_shims: bool,
) -> Vec<EcosystemEntry> {
    let mut seen = Vec::new();
    for pm in &ctx.package_managers() {
        let eco = pm.ecosystem();
        if !seen.contains(&eco) {
            seen.push(eco);
        }
    }
    if decisions.has_node_context(ctx) && !seen.contains(&Ecosystem::Node) {
        seen.push(Ecosystem::Node);
    }
    if decisions.has_python_context(ctx) && !seen.contains(&Ecosystem::Python) {
        seen.push(Ecosystem::Python);
    }

    seen.into_iter()
        .map(|eco| match eco {
            Ecosystem::Node => node_ecosystem(ctx, decisions, resolve_shims),
            Ecosystem::Python => decided_ecosystem(
                ctx,
                Ecosystem::Python,
                decisions,
                detected_pm_signals(ctx, Ecosystem::Python),
            ),
            other => single_pm_ecosystem(ctx, other),
        })
        .collect()
}

fn node_ecosystem(
    ctx: &ProjectContext,
    decisions: &Decisions,
    resolve_shims: bool,
) -> EcosystemEntry {
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
        "manifest_pm": decisions.manifest.as_ref().map(|d| d.pm.label()),
        "path_probe": probes.path_probe,
        "shims": shims,
    });
    decided_ecosystem(ctx, Ecosystem::Node, decisions, signals)
}

/// An ecosystem whose scripts are dispatched by a chosen package manager.
fn decided_ecosystem(
    ctx: &ProjectContext,
    eco: Ecosystem,
    decisions: &Decisions,
    signals: serde_json::Value,
) -> EcosystemEntry {
    let (decision, selected) = decisions.for_ecosystem(eco).map_or_else(
        || {
            (
                EcosystemDecision {
                    confidence: Confidence::None,
                    reason: decisions
                        .error
                        .clone()
                        .unwrap_or_else(|| format!("no {} package manager detected", eco.label())),
                    selected: None,
                },
                None,
            )
        },
        |decision| {
            let label = decision.pm.label();
            (
                EcosystemDecision {
                    confidence: if decision.probed() {
                        Confidence::Medium
                    } else {
                        Confidence::High
                    },
                    reason: decision.describe(),
                    selected: Some(label),
                },
                Some(label),
            )
        },
    );

    EcosystemEntry {
        decision,
        name: eco.label(),
        root: ctx.root.display().to_string(),
        selected_package_manager: selected,
        signals,
    }
}

/// Single-PM ecosystems (rust/go/deno/ruby/php): the detected manager
/// *is* the decision; there is no competing-PM resolution chain.
fn single_pm_ecosystem(ctx: &ProjectContext, eco: Ecosystem) -> EcosystemEntry {
    let selected = ctx
        .package_managers()
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
            .package_managers()
            .iter()
            .filter(|pm| pm.ecosystem() == eco)
            .map(|pm| pm.label())
            .collect::<Vec<_>>(),
    })
}

fn sources(ctx: &ProjectContext) -> Vec<SourceEntry<'_>> {
    let mut seen: Vec<&Task> = Vec::new();
    for task in &ctx.tasks {
        if !seen
            .iter()
            .any(|first| first.source == task.source && first.same_scope(task))
        {
            seen.push(task);
        }
    }

    seen.into_iter()
        .map(|task| {
            let source = task.source;
            let kind = StructuredSource(source);
            let anchor = super::labels::source_anchor(source, task.dir(&ctx.root));
            let path = anchor
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string());
            let relpath = anchor.as_ref().map_or_else(String::new, |p| {
                p.strip_prefix(&ctx.root).unwrap_or(p).display().to_string()
            });
            SourceEntry {
                exists: anchor.as_ref().is_some_and(|p| p.is_file()),
                id: format!("src:{}:{}", task.scope(), structured_source_label(source)),
                kind,
                package: task
                    .member
                    .as_ref()
                    .map(|member| serde_json::json!({ "name": member.name, "path": member.path })),
                path,
                relpath,
                scope: task.scope(),
                task_pointer: task_container_key(source),
            }
        })
        .collect()
}

fn tasks<'a>(ctx: &'a ProjectContext, overrides: &ResolutionOverrides) -> Vec<DoctorTask<'a>> {
    let prepared = crate::commands::run::core::prepare(ctx, overrides, "");

    // `anchor_file` walks the filesystem; resolve each distinct
    // (scope, source) pair once instead of once per task.
    let mut anchors: std::collections::HashMap<(&str, TaskSource), Option<String>> =
        std::collections::HashMap::new();
    for task in &ctx.tasks {
        anchors
            .entry((task.scope(), task.source))
            .or_insert_with(|| {
                super::labels::source_anchor(task.source, task.dir(&ctx.root))
                    .map(|p| p.display().to_string())
            });
    }

    ctx.tasks
        .iter()
        .map(|task| DoctorTask {
            aliases: ctx
                .tasks
                .iter()
                .filter(|other| {
                    other.source == task.source
                        && other.same_scope(task)
                        && other.alias_of.as_deref() == Some(&task.name)
                })
                .map(|other| other.name.as_str())
                .collect(),
            cwd: task.run_dir(&ctx.root).display().to_string(),
            definition: task.alias_of.as_deref().or(task.run_target.as_deref()),
            dependencies: task.detail.depends.iter().map(String::as_str).collect(),
            description: task.description.as_deref(),
            fqn: super::labels::fqn(task),
            is_alias: task.alias_of.is_some(),
            name: &task.name,
            resolved: prepared
                .as_ref()
                .ok()
                .and_then(|prepared| {
                    prepared
                        .preview(ctx, overrides, &super::labels::fqn(task))
                        .ok()
                })
                .and_then(|(_, dispatch)| match dispatch {
                    runner_core::Dispatch::Plan(plan) => {
                        Some(super::labels::planned_command(&plan))
                    }
                    runner_core::Dispatch::Builtin(_) => None,
                }),
            scope: task.scope(),
            self_executable: false,
            source: anchors.get(&(task.scope(), task.source)).cloned().flatten(),
            source_pointer: super::labels::source_pointer(task),
        })
        .collect()
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

fn tools(ctx: &ProjectContext, decisions: &Decisions) -> Vec<Tool> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var_os("PATHEXT");
    let pathext_ref = pathext.as_deref();

    let mut tools = Vec::new();

    if decisions.has_node_context(ctx) {
        tools.push(probe_tool(
            "node",
            DependencyKind::Runtime,
            ctx.current_node()
                .map(|v| v.trim_start_matches('v').to_string()),
            true,
            &path,
            pathext_ref,
        ));
    }
    // Same reasoning as the node runtime probe above: a resolved
    // `uv run <task>` must never reference an interpreter the tools
    // surface claims absent.
    if decisions.has_python_context(ctx) {
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

    for pm in &ctx.package_managers() {
        let required = true;
        tools.push(probe_tool(
            pm_binary_name(*pm),
            DependencyKind::PackageManager,
            None,
            required,
            &path,
            pathext_ref,
        ));
    }
    for runner in &ctx.task_runners() {
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
    observed: Option<&Observed>,
    plan: Option<&InstallPlan>,
) -> Vec<Conflict> {
    // Grouped per scope: root and member tasks sharing a name are not in
    // conflict (root wins; the member stays reachable as `member:name`).
    let mut by_name: BTreeMap<(&str, &str), Vec<&Task>> = BTreeMap::new();
    for task in &ctx.tasks {
        by_name
            .entry((task.scope(), &task.name))
            .or_default()
            .push(task);
    }

    let duplicate_names = by_name
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .filter_map(|((_, name), group)| {
            let observed = observed?;
            let selected = observed.winner(ctx, &group)?;
            let ranked = observed.ranked(ctx, &group);
            let fqn_of = |task: &Task| super::labels::fqn(task);
            let reason = format!(
                "{count} sources define `{name}`; {source} runs because {why}",
                count = group.len(),
                source = selected.source.label(),
                why = crate::commands::run::core::rank_reason(&observed.policy, &ranked),
            );
            Some(Conflict::DuplicateTaskName {
                reason,
                selected: fqn_of(selected),
                selector: selected.display_name().into_owned(),
                severity: Severity::Info,
                shadowed: group
                    .iter()
                    .filter(|task| !std::ptr::eq(**task, selected))
                    .map(|task| fqn_of(task))
                    .collect(),
            })
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
        .map(|shadow| Conflict::InstallDirCollision {
            reason: format!(
                "{} and {} both install into {}/; the package manager resolved for the ecosystem \
                 installs it and the other is skipped. Enable both with `[tools.<name>].install = \
                 true` to run them sequentially.",
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

/// Run the health checks declared by present providers.
pub(crate) fn provider_diagnostics(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Vec<Diagnostic> {
    let tree = crate::commands::run::core::tree(ctx);
    let policy = crate::commands::run::core::policy(overrides);
    let project = match crate::commands::run::core::project(ctx) {
        Ok(project) => project,
        Err(error) => {
            return vec![Diagnostic {
                code: "observation",
                message: error.to_string(),
                severity: Severity::Warning,
                source: None,
                task: None,
            }];
        }
    };
    let mut diagnostics = Vec::new();
    for present in &project.present {
        let provider = runner_providers::REGISTRY
            .by_id(present.provider)
            .for_present(present);
        for index in 0..provider.caps.health.len() {
            let messages = match runner_core::health::check(
                &tree,
                &project,
                &policy,
                present,
                index,
                &runner_providers::REGISTRY,
            ) {
                Ok(runner_core::Health::Ok) => Vec::new(),
                Ok(runner_core::Health::Problems(messages)) => messages,
                Ok(runner_core::Health::Unreadable(message)) => vec![message],
                Err(error) => vec![error.to_string()],
            };
            diagnostics.extend(messages.into_iter().map(|message| Diagnostic {
                code: "health",
                message,
                severity: Severity::Warning,
                source: Some(provider.label),
                task: None,
            }));
        }
    }
    diagnostics
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

    use super::{DoctorReport, rfc3339_utc};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{Ecosystem, PackageManager, ProjectContext, Task, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        crate::tool::test_support::write_signal(&root, PackageManager::Cargo.label());
        let mut ctx = ProjectContext {
            cwd: root.clone(),
            root,
            tasks,
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        };
        crate::tool::test_support::seed_context(&mut ctx);
        ctx
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
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
        assert_eq!(json["overrides"]["output"]["level"], "off");
        assert_eq!(json["overrides"]["output"]["fatal_errors"], true);
        assert_eq!(json["overrides"]["quiet"], false);
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
        assert_eq!(task["resolved"], "cargo t");
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
            !ctx.package_managers()
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
            !ctx.package_managers()
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
    fn forced_runtime_previews_package_json_scripts_through_the_runtime() {
        // A forced runtime dispatches package.json scripts through its own
        // runner; `resolved` must match that, not the resolved PM's command.
        let overrides = ResolutionOverrides::from_cli_and_env(
            crate::resolver::CliOverrides {
                runtime: Some("bun"),
                ..crate::resolver::CliOverrides::default()
            },
            crate::resolver::DiagnosticFlags::default(),
            crate::args::ChainFailureFlags::default(),
            None,
        )
        .expect("runtime override should parse");
        let mut ctx = context(vec![task("build", TaskSource::PackageJson)]);
        crate::tool::test_support::declare(&mut ctx, PackageManager::Bun.label());
        crate::tool::test_support::seed_context(&mut ctx);
        let report = DoctorReport::build(&ctx, &overrides, false);
        let json = serde_json::to_value(&report).expect("report should serialize");

        let build = json["tasks"]
            .as_array()
            .expect("tasks array")
            .iter()
            .find(|t| t["name"] == "build")
            .expect("build task present");
        assert_eq!(build["resolved"], "bun --bun run build");
        assert_eq!(json["overrides"]["runtime"], "bun");
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

        let overrides = ResolutionOverrides {
            failure_policy: FailurePolicy::KeepGoing,
            grouping: crate::resolver::OutputGrouping {
                group_output: false,
                github_group_parallel: false,
                parallel_grouped: true,
            },
            tool_install: [
                ("npm".into(), vec!["install".into()]),
                ("pnpm".into(), Vec::new()),
            ]
            .into(),
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
        assert_eq!(
            ov["tool_install"],
            serde_json::json!({"npm": ["install"], "pnpm": []})
        );
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
    /// Fields reported under another name are listed in `RENAMED`.
    #[test]
    fn every_resolution_overrides_field_is_reported_or_excluded() {
        // Internal runner-to-runner plumbing (inherited env markers),
        // never user overrides, nothing meaningful to report.
        // `parent_*` are internal runner-to-runner plumbing. `host_stream` and
        // `task_verbosity` are the host-tool verbosity knobs, which affect the
        // spawned tool's flags rather than runner's own resolution, so they're
        // not part of the doctor resolution report (the runner-facing quiet
        // level still is, as `quiet`).
        const EXCLUDED: &[&str] = &[
            "package",
            "parent",
            "host_stream",
            "task_verbosity",
            "host_diagnostics_explicit",
        ];
        // Resolver field name -> name it's actually reported under.
        const RENAMED: &[(&str, &str)] = &[
            ("reach", "fetch"),
            ("grouping", "output_grouping"),
            ("task_source_overrides", "task_source_pins"),
            ("quiet_level", "quiet"),
            ("output_policy", "output"),
            ("host_stream_config", "output"),
        ];

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
            package,
            pm_by_ecosystem,
            runner,
            runtime,
            prefer_runners,
            prefer_sources,
            task_source_overrides,
            fallback,
            on_mismatch,
            no_warnings,
            quiet_level,
            host_diagnostics_explicit,
            output_policy,
            host_stream,
            host_stream_config,
            task_verbosity,
            explain,
            failure_policy,
            grouping,
            script_policy,
            on_collision,
            parent,
            env,
            tool_install,
            reach,
            lockfile,
        ];

        let schema = serde_json::to_value(schemars::schema_for!(super::Overrides))
            .expect("Overrides schema should serialize");
        let top_properties = schema["properties"]
            .as_object()
            .expect("Overrides schema must have properties");
        let reported: std::collections::BTreeSet<&str> =
            top_properties.keys().map(String::as_str).collect();

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
