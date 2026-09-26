//! Typed JSON shapes for `--json` output across `info`, `list`, and `doctor`'s human renderer.
//!
//! Every one of those surfaces projects from the single source-of-truth [`Project`] struct so the
//! contract is defined in one place: `list` emits [`Project::into_list_view`], `info` emits
//! [`Project::into_info_view`], and `doctor`'s human (non-JSON) output reads this shape internally
//! even though its `--json` output is the structured [`super::doctor::DoctorReport`] instead.

use std::collections::BTreeMap;

use serde::Serialize;

use super::labels::SourceLabel;
use crate::commands::run::decision::Observed;
use crate::provider::Named;
use crate::resolver::{OverrideOrigin, ResolutionOverrides};
use crate::types::{DetectionWarning, ProjectContext, Workspace};
use runner_core::ProviderId;

/// The canonical machine-readable view of a project, used by every `--json` surface. Field order is
/// preserved by `serde_json` so consumers can hand-write `jq` queries without sort surprises.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Project<'a> {
    /// URI of the JSON Schema that describes this payload.
    #[serde(rename = "$schema", skip_serializing_if = "str::is_empty")]
    #[schemars(description = "URI of the JSON Schema that describes this payload.")]
    pub schema: String,
    /// Increments on any breaking change to this schema.
    /// Consumers should reject anything they weren't built for.
    #[schemars(description = "Schema contract version for this JSON payload.")]
    pub schema_version: u32,
    /// Absolute path of the project root the report describes.
    pub root: String,
    /// Detected ecosystems, in the order their package managers were
    /// found by [`crate::detect`].
    pub ecosystems: Vec<&'static str>,
    /// Raw, type-deduplicated detection results: PMs, runners, Node
    /// version, monorepo flag. Stable across resolver behavior tweaks.
    pub detected: Detected<'a>,
    /// Effective override stack, CLI, env, and config bundled.
    pub overrides: OverridesView,
    /// Detection signals per task source that package managers dispatch, keyed by source label.
    pub signals: BTreeMap<&'static str, SourceSignals>,
    /// The package manager that dispatches each such task source, or why there is none.
    pub decisions: BTreeMap<&'static str, Decision>,
    /// Full task list. Subcommands that don't care omit this via projection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskInfo<'a>>,
    /// Diagnostic warnings from detection and from the package-manager decision, flattened.
    pub warnings: Vec<WarningInfo>,
}

impl<'a> Project<'a> {
    /// Build the full report. Test-only convenience. Production callers go through the dispatcher,
    /// which validates `--schema-version` and calls [`Self::build_with_schema`] directly.
    #[cfg(test)]
    pub(crate) fn build(ctx: &'a ProjectContext, overrides: &ResolutionOverrides) -> Self {
        // `resolve_shims = false` keeps unit tests hermetic, no `volta which` spawns against the test host.
        Self::build_with_schema(ctx, overrides, false)
    }

    /// Build the report. `resolve_shims` controls whether PATH-probe hits are classified against a
    /// Volta installation (one `volta which` spawn per shimmed tool). Diagnostic surfaces
    /// (`doctor`, `info --json`) pass `true`; `list` passes `false` because it drops signals anyway.
    pub(crate) fn build_with_schema(
        ctx: &'a ProjectContext,
        overrides: &ResolutionOverrides,
        resolve_shims: bool,
    ) -> Self {
        let observed = Observed::observe(ctx, overrides);
        let sources = dispatched_sources(ctx, observed.as_ref().ok());
        let (decisions, resolver_warnings) = decisions_for(&observed, &sources);

        let warnings = ctx
            .warnings
            .iter()
            .chain(resolver_warnings.iter())
            .map(WarningInfo::from_warning)
            .collect();

        let tasks = ctx
            .tasks
            .iter()
            .map(|t| TaskInfo {
                name: &t.name,
                source: SourceLabel(t.source),
                member: t.member.as_ref().map(|member| member.name.as_str()),
                description: t.description.as_deref(),
                alias_of: t.alias_of.as_deref(),
                passthrough_to: t.passthrough_to.map(Named::label),
                depends: t.detail.depends.iter().map(String::as_str).collect(),
                dir: t.detail.dir.as_ref().map(|dir| dir.display().to_string()),
                usage: t.detail.usage.as_deref(),
            })
            .collect();

        Self {
            schema: String::new(),
            schema_version: super::SCHEMA_VERSION,
            root: ctx.root.display().to_string(),
            ecosystems: ctx
                .package_managers()
                .iter()
                .map(|pm| pm.ecosystem().label())
                .collect(),
            detected: Detected::from_ctx(ctx),
            overrides: OverridesView::from_resolution_overrides(overrides),
            signals: sources
                .iter()
                .map(|&source| {
                    (
                        source.label(),
                        source_signals(ctx, observed.as_ref().ok(), source, resolve_shims),
                    )
                })
                .collect(),
            decisions,
            tasks,
            warnings,
        }
    }

    /// Project the full report to an `info`-shaped view: same shape minus the per-task detail
    /// (which `info` doesn't need; `list` is the dedicated task surface).
    pub(crate) fn into_info_view(mut self) -> Self {
        self.tasks.clear();
        self
    }

    /// Project the full report to a `list`-shaped view: just the tasks (filtered by `source` when set)
    /// plus the schema version and root. Drops resolver state because `list` is purely a directory listing for tasks.
    pub(crate) fn into_list_view(self, only: &[ProviderId]) -> TaskListView<'a> {
        let tasks = self
            .tasks
            .into_iter()
            .filter(|t| {
                only.is_empty() || only.iter().any(|source| SourceLabel(*source) == t.source)
            })
            .collect();
        TaskListView {
            schema: String::new(),
            schema_version: self.schema_version,
            root: self.root,
            tasks,
        }
    }
}

/// `list --json` projection. Same `schema_version` as [`Project`] so consumers can branch on it.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[schemars(
    title = "runner list --json",
    description = "JSON schema for `runner list --json`.",
    extend("$id" = super::schema_url("list"))
)]
pub(crate) struct TaskListView<'a> {
    /// URI of the JSON Schema that describes this payload.
    #[serde(rename = "$schema", skip_serializing_if = "str::is_empty")]
    #[schemars(description = "URI of the JSON Schema that describes this payload.")]
    pub schema: String,
    /// Identical to [`Project::schema_version`]; consumers can branch on the
    /// unified output contract version.
    #[schemars(
        description = "Schema contract version for this JSON payload.",
        extend("const" = super::SCHEMA_VERSION)
    )]
    pub schema_version: u32,
    /// Project root.
    pub root: String,
    /// Tasks, optionally filtered by source.
    pub tasks: Vec<TaskInfo<'a>>,
}

/// Detection results, what the file scan found, before any resolver policy was applied.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Detected<'a> {
    /// Detected package managers, in detection-priority order.
    pub package_managers: Vec<&'static str>,
    /// Detected task runners.
    pub task_runners: Vec<&'static str>,
    /// Runtimes the root declares, with their expected and installed versions.
    pub runtimes: Vec<RuntimeInfo>,
    /// Whether the project declares a workspace.
    pub monorepo: bool,
    /// Workspace declarations at the root and their members. Additive field
    /// (no schema bump): absent when the root declares no workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceInfo<'a>>,
}

impl<'a> Detected<'a> {
    fn from_ctx(ctx: &'a ProjectContext) -> Self {
        Self {
            package_managers: ctx.package_managers().iter().map(|pm| pm.label()).collect(),
            task_runners: ctx.task_runners().iter().map(|tr| tr.label()).collect(),
            runtimes: ctx
                .runtime_versions()
                .into_iter()
                .map(|runtime| RuntimeInfo {
                    name: runtime.runtime.label(),
                    expected: runtime.expected.map(|expected| ExpectedVersionInfo {
                        version: expected.version,
                        source: expected.source,
                    }),
                    current: runtime.current,
                })
                .collect(),
            monorepo: ctx.is_monorepo(),
            workspace: ctx.workspace.as_ref().map(WorkspaceInfo::from_workspace),
        }
    }
}

/// Workspace declarations and members projected into the JSON shape.
#[derive(schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceInfo<'a> {
    /// Absolute directory holding the declarations.
    pub root: String,
    /// Declaration labels in detection order (e.g. `"package.json workspaces"`).
    pub kinds: Vec<&'static str>,
    /// Members in path order.
    pub members: Vec<WorkspaceMemberInfo<'a>>,
    /// Name of the member the invocation directory sits in, if any.
    pub current: Option<&'a str>,
}

impl<'a> WorkspaceInfo<'a> {
    pub(crate) fn from_workspace(workspace: &'a Workspace) -> Self {
        Self {
            root: workspace.root.display().to_string(),
            current: workspace
                .current
                .as_ref()
                .map(|member| member.name.as_str()),
            kinds: workspace.kinds.clone(),
            members: workspace
                .members
                .iter()
                .map(|member| WorkspaceMemberInfo {
                    name: &member.name,
                    path: &member.path,
                    dir: member.dir.display().to_string(),
                })
                .collect(),
        }
    }
}

/// One workspace member.
#[derive(schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceMemberInfo<'a> {
    /// Manifest name, or the directory name when the manifest declares none.
    pub name: &'a str,
    /// Directory relative to the workspace root, forward slashes.
    pub path: &'a str,
    /// Absolute directory.
    pub dir: String,
}

/// A runtime the root declares.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct RuntimeInfo {
    /// Runtime label.
    pub name: &'static str,
    /// The version the project declares.
    pub expected: Option<ExpectedVersionInfo>,
    /// The installed version.
    pub current: Option<String>,
}

/// A declared runtime version plus the file or field that declares it.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ExpectedVersionInfo {
    /// Version string as written (e.g. `"20.11.0"`, `">=18"`).
    pub version: String,
    /// Where it is declared (e.g. `".nvmrc"`, `"package.json engines.node"`).
    pub source: String,
}

/// The provider choices the command line, the environment and `runner.toml`
/// made, each with its origin.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct OverridesView {
    /// `--pm` / `RUNNER_PM`.
    pub pm: Option<ChoiceInfo>,
    /// `--source` / `RUNNER_SOURCE`.
    pub source: Option<ChoiceInfo>,
    /// `--runtime` / `RUNNER_RUNTIME` / `[runtime].javascript`.
    pub runtime: Option<ChoiceInfo>,
    /// Whether `--dry-run` is on.
    pub dry_run: bool,
    /// Whether warnings print.
    pub warnings: bool,
}

impl OverridesView {
    fn from_resolution_overrides(overrides: &ResolutionOverrides) -> Self {
        let choice = |label: &'static str, origin: &OverrideOrigin| ChoiceInfo {
            value: label,
            origin: origin_label(origin),
        };
        Self {
            pm: overrides
                .pm
                .as_ref()
                .map(|o| choice(o.pm.label(), &o.origin)),
            source: overrides
                .source
                .as_ref()
                .map(|o| choice(o.source.label(), &o.origin)),
            runtime: overrides
                .runtime
                .as_ref()
                .map(|o| choice(o.runtime.label(), &o.origin)),
            dry_run: overrides.dry_run,
            warnings: overrides.shows_warnings(),
        }
    }
}

/// A chosen provider and where the choice came from.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ChoiceInfo {
    /// The provider's label.
    pub value: &'static str,
    /// `"cli"`, `"env"`, `"config:/abs/path"` or `"config:/abs/path#tasks.<name>"`.
    pub origin: String,
}

/// What the resolver had to work with for one dispatched task source.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct SourceSignals {
    /// Package manager a lockfile pins, if any.
    pub lockfile_pm: Option<&'static str>,
    /// Manifest declaration of the package manager.
    pub manifest_pm: Option<ManifestPm>,
    /// Each package manager that dispatches the source -> absolute path on `$PATH` (or null).
    pub path_probe: BTreeMap<&'static str, Option<String>>,
    /// PATH-probe hits that are version-manager shims, keyed like [`Self::path_probe`]. Absent
    /// without shims and on surfaces that skip shim resolution.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub shims: BTreeMap<&'static str, ShimInfo>,
}

/// What a version manager said about one shimmed tool.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ShimInfo {
    /// The version manager that owns the shim.
    pub manager: &'static str,
    /// Real provisioned binary behind the shim; `null` when the manager has no version of the
    /// tool. Shims the manager could not classify are omitted from the map.
    pub resolved: Option<String>,
}

/// Manifest-level PM declaration plus the field it came from.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ManifestPm {
    /// Declared PM label.
    pub pm: &'static str,
    /// The manifest field, e.g. `"packageManager"` or `"devEngines.packageManager"`.
    pub source: &'static str,
    /// Version constraint as written, if present.
    pub version: Option<String>,
    /// Effective `onFail` policy (`"ignore"`, `"warn"`, `"error"`).
    pub on_fail: &'static str,
}

/// Either a resolved package manager or the diagnostic string for the failure that prevented one.
///
/// Untagged so consumers can probe via "is the `pm` field present?".
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Decision {
    /// Successful resolution.
    Resolved {
        /// The chosen PM label.
        pm: &'static str,
        /// Human-readable `via` line, the same string `--dry-run` prints.
        via: String,
    },
    /// No package manager dispatches the source here, or observation failed.
    Error {
        /// One-line description.
        error: String,
    },
}

/// Task entry projected into the JSON shape.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskInfo<'a> {
    /// Task name as it appears in the config.
    pub name: &'a str,
    /// Label of the task's source.
    pub source: SourceLabel,
    /// Workspace member the task belongs to; absent for root tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<&'a str>,
    /// Human-readable description, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    /// When the task is an alias, the target it resolves to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<&'a str>,
    /// When the task's body is a thin wrapper for another runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passthrough_to: Option<&'static str>,
    /// Tasks that run before this one, as the source declares them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<&'a str>,
    /// Directory the task executes in, when the source declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Argument and flag spec in the source's own language (mise: usage KDL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<&'a str>,
}

/// Warning projected into the JSON shape. The `source`/`detail` split is kept stable from the
/// pre-A4 flat-struct days so existing consumers (the `doctor` test suite, ad-hoc `jq` queries) keep working.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct WarningInfo {
    /// Subsystem the warning came from (e.g. `"package.json"`).
    pub source: &'static str,
    /// Human-readable detail.
    pub detail: String,
}

impl WarningInfo {
    fn from_warning(warning: &DetectionWarning) -> Self {
        Self {
            source: warning.source(),
            detail: warning.detail(),
        }
    }
}

/// The task sources package managers dispatch that the project uses: a
/// package manager dispatches them here, or the project defines their tasks.
pub(crate) fn dispatched_sources(
    ctx: &ProjectContext,
    observed: Option<&Observed>,
) -> Vec<ProviderId> {
    let pms = ctx.package_managers();
    crate::provider::managed_sources()
        .into_iter()
        .filter(|&source| {
            observed.is_some_and(|observed| observed.decision(source).is_some())
                || pms.iter().any(|pm| pm.dispatches().contains(&source))
                || ctx.tasks.iter().any(|task| task.source == source)
        })
        .collect()
}

/// The lockfile, manifest and `PATH` signals for `source`.
pub(crate) fn source_signals(
    ctx: &ProjectContext,
    observed: Option<&Observed>,
    source: ProviderId,
    resolve_shims: bool,
) -> SourceSignals {
    let probes = probe_signals(&ctx.root, source, resolve_shims);
    SourceSignals {
        lockfile_pm: observed
            .and_then(|observed| observed.locked(source))
            .map(Named::label),
        manifest_pm: observed
            .and_then(|observed| observed.manifest_declaration(source))
            .map(|d| ManifestPm {
                pm: d.pm.label(),
                source: d.field,
                version: d.version,
                on_fail: d.on_fail.label(),
            }),
        path_probe: probes.path_probe,
        shims: probes.shims,
    }
}

fn decisions_for(
    observed: &std::io::Result<Observed>,
    sources: &[ProviderId],
) -> (BTreeMap<&'static str, Decision>, Vec<DetectionWarning>) {
    let mut decisions = BTreeMap::new();
    let mut warnings = Vec::new();
    for &source in sources {
        let decision = match observed {
            Ok(observed) => match observed.decision(source) {
                Some(decision) => {
                    warnings.extend(decision.warnings(&observed.project));
                    Decision::Resolved {
                        pm: decision.pm.label(),
                        via: decision.describe(),
                    }
                }
                None => Decision::Error {
                    error: crate::provider::no_dispatcher(source),
                },
            },
            Err(error) => Decision::Error {
                error: error.to_string(),
            },
        };
        decisions.insert(source.label(), decision);
    }
    (decisions, warnings)
}

fn origin_label(origin: &OverrideOrigin) -> String {
    match origin {
        OverrideOrigin::CliFlag => "cli".to_string(),
        OverrideOrigin::EnvVar => "env".to_string(),
        OverrideOrigin::ConfigFile { path } => format!("config:{}", path.display()),
        OverrideOrigin::TaskConfig { path, task } => {
            format!("config:{}#tasks.{task}", path.display())
        }
    }
}

/// Every `PATH` hit for the package managers that dispatch a source, plus
/// shim classification when requested.
pub(super) struct ProbeSignals {
    pub(super) path_probe: BTreeMap<&'static str, Option<String>>,
    pub(super) shims: BTreeMap<&'static str, ShimInfo>,
}

/// Probe each package manager that dispatches `source` and report
/// `(label, path)` pairs, one thread per probe.
pub(super) fn probe_signals(
    root: &std::path::Path,
    source: ProviderId,
    resolve_shims: bool,
) -> ProbeSignals {
    use std::env;
    use std::thread;

    let path = env::var_os("PATH").unwrap_or_default();
    let pathext = env::var_os("PATHEXT");
    let pathext_ref = pathext.as_deref();
    let managers: Vec<(&'static str, runner_core::ShimsCap, Vec<std::path::PathBuf>)> =
        if resolve_shims {
            runner_providers::REGISTRY
                .iter()
                .filter_map(|provider| {
                    let shims = provider.caps.shims?;
                    Some((provider.label, shims, (shims.dirs)()))
                })
                .filter(|(_, _, dirs)| !dirs.is_empty())
                .collect()
        } else {
            Vec::new()
        };

    thread::scope(|s| {
        let order = crate::provider::dispatchers(source);
        let mut handles = Vec::with_capacity(order.len());
        for pm in order {
            let path = &path;
            let managers = &managers;
            handles.push(s.spawn(move || {
                let program = pm.provider().program.unwrap_or_else(|| pm.label());
                let resolved = crate::resolver::probe_path_for_doctor(program, path, pathext_ref);
                let shim = resolved.as_deref().and_then(|hit| {
                    let parent = hit.parent()?;
                    let parent = parent.canonicalize().unwrap_or_else(|_| parent.to_owned());
                    let (manager, cap, _) = managers
                        .iter()
                        .find(|(_, _, dirs)| dirs.contains(&parent))?;
                    Some((*manager, (cap.resolve)(program, root)))
                });
                (pm.label(), resolved.map(|p| p.display().to_string()), shim)
            }));
        }

        let mut path_probe = BTreeMap::new();
        let mut shims = BTreeMap::new();
        for handle in handles {
            let (label, resolved, shim) = handle.join().expect("path probe thread panicked");
            path_probe.insert(label, resolved);
            let (manager, resolved) = match shim {
                Some((manager, runner_core::Shim::Resolved(real))) => {
                    (manager, Some(real.display().to_string()))
                }
                Some((manager, runner_core::Shim::NotProvisioned)) => (manager, None),
                Some((_, runner_core::Shim::Unknown)) | None => continue,
            };
            shims.insert(label, ShimInfo { manager, resolved });
        }
        ProbeSignals { path_probe, shims }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Project;
    use crate::resolver::ResolutionOverrides;
    use crate::types::{ProjectContext, Task};
    use runner_core::ProviderId;

    fn pnpm_context() -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        crate::tool::test_support::write_signal(&root, ProviderId::Pnpm);
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
    fn project_serializes_schema_version_field() {
        let ctx = pnpm_context();
        let overrides = ResolutionOverrides::default();
        let project = Project::build(&ctx, &overrides);
        let value = serde_json::to_value(&project).expect("Project should serialize to JSON");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["root"], ctx.root.to_str().unwrap());
        assert!(
            value["ecosystems"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
    }

    #[test]
    fn info_view_drops_tasks_array() {
        let mut ctx = pnpm_context();
        ctx.tasks.push(Task {
            name: "build".to_string(),
            source: ProviderId::PackageJson,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        });
        let project = Project::build(&ctx, &ResolutionOverrides::default()).into_info_view();
        let value = serde_json::to_value(&project).expect("info view should serialize");

        // `skip_serializing_if = Vec::is_empty` collapses to no field.
        assert!(value.get("tasks").is_none(), "info view should omit tasks");
    }

    #[test]
    fn list_view_filters_by_source() {
        let mut ctx = pnpm_context();
        ctx.tasks.push(Task {
            name: "build".to_string(),
            source: ProviderId::PackageJson,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        });
        ctx.tasks.push(Task {
            name: "fmt".to_string(),
            source: ProviderId::Just,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        });
        let project = Project::build(&ctx, &ResolutionOverrides::default());
        let view = project.into_list_view(&[ProviderId::Just]);

        assert_eq!(view.tasks.len(), 1);
        assert_eq!(view.tasks[0].name, "fmt");
    }

    #[test]
    fn build_with_schema_serializes_flat_labels_for_tasks() {
        let ctx = ProjectContext {
            cwd: PathBuf::from("/tmp/test"),
            root: PathBuf::from("/tmp/test"),
            tasks: vec![Task {
                name: "fmt".to_string(),
                source: ProviderId::Just,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
                detail: crate::types::TaskDetail::default(),
                member: None,
            }],
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        };

        let project = Project::build_with_schema(&ctx, &ResolutionOverrides::default(), false);
        let json = serde_json::to_value(&project).expect("serialization");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["tasks"][0]["source"], "just");
    }
}
