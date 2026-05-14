//! Typed schema for `--json` output across `doctor`, `info`, `list`,
//! and `why`.
//!
//! Per the resolved design decision in
//! `plans/eager-squishing-peacock.md` (A4), every subcommand projects
//! from a single source-of-truth [`Project`] struct so the JSON
//! contract is defined in one place. Doctor emits the full struct;
//! info/list emit projections (currently the full shape with empty
//! task tables collapsed away by `#[serde(skip_serializing_if)]`).
//!
//! The schema is versioned via [`Project::SCHEMA_VERSION`]; bumping it
//! is a breaking change to the public JSON contract.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::resolver::{
    FallbackPolicy, MismatchPolicy, OverrideOrigin, ResolutionOverrides, Resolver,
};
use crate::tool::node::{ManifestSource, detect_pm_from_manifest};
use crate::types::{DetectionWarning, PackageManager, ProjectContext, TaskSource};

/// The canonical machine-readable view of a project, used by every
/// `--json` surface. Field order is preserved by `serde_json` so
/// consumers can hand-write `jq` queries without sort surprises.
#[derive(Debug, Serialize)]
pub(crate) struct Project<'a> {
    /// Increments on any breaking change to this schema. Consumers
    /// should reject anything they weren't built for.
    pub schema_version: u32,
    /// Absolute path of the project root the report describes.
    pub root: String,
    /// Detected ecosystems, in the order their package managers were
    /// found by [`crate::detect`].
    pub ecosystems: Vec<&'static str>,
    /// Raw, type-deduplicated detection results: PMs, runners, Node
    /// version, monorepo flag. Stable across resolver behavior tweaks.
    pub detected: Detected<'a>,
    /// Effective override stack — CLI, env, and config bundled.
    pub overrides: OverridesView,
    /// Per-ecosystem detection signals: lockfile pick, manifest
    /// declaration, PATH probe results.
    pub signals: Signals,
    /// Resolver verdict (or first-class error if the chain bailed).
    pub decisions: Decisions,
    /// Full task list. Subcommands that don't care omit this via
    /// projection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskInfo<'a>>,
    /// Diagnostic warnings from both detection (`ctx.warnings`) and
    /// the resolver (`ResolvedPm.warnings`), flattened.
    pub warnings: Vec<WarningInfo>,
}

impl<'a> Project<'a> {
    /// Schema version for the JSON contract. Bump on any breaking change.
    pub(crate) const SCHEMA_VERSION: u32 = 1;

    /// Build the full report from a project context + resolver overrides.
    /// Runs the resolver chain once so `decisions` reflects the current
    /// override stack.
    pub(crate) fn build(ctx: &'a ProjectContext, overrides: &ResolutionOverrides) -> Self {
        let manifest_decl = detect_pm_from_manifest(&ctx.root);
        let manifest_pm = manifest_decl.as_ref().map(|d| ManifestPm {
            pm: d.pm.label(),
            source: match d.source {
                ManifestSource::PackageManager => "packageManager",
                ManifestSource::DevEngines => "devEngines.packageManager",
            },
            version: d.version.clone(),
            on_fail: d.on_fail.label(),
        });

        let (decisions, resolver_warnings) = decisions_for(ctx, overrides);

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
                source: t.source.label(),
                description: t.description.as_deref(),
                alias_of: t.alias_of.as_deref(),
                passthrough_to: t.passthrough_to.map(crate::types::TaskRunner::label),
            })
            .collect();

        Self {
            schema_version: Self::SCHEMA_VERSION,
            root: ctx.root.display().to_string(),
            ecosystems: ctx
                .package_managers
                .iter()
                .map(|pm| pm.ecosystem().label())
                .collect(),
            detected: Detected::from_ctx(ctx),
            overrides: OverridesView::from_resolution_overrides(overrides),
            signals: Signals {
                node: NodeSignals {
                    lockfile_pm: ctx.primary_node_pm().map(PackageManager::label),
                    manifest_pm,
                    path_probe: path_probe_map(),
                },
            },
            decisions,
            tasks,
            warnings,
        }
    }

    /// Project the full report to an `info`-shaped view: same shape
    /// minus the per-task detail (which `info` doesn't need; `list` is
    /// the dedicated task surface).
    pub(crate) fn into_info_view(mut self) -> Self {
        self.tasks.clear();
        self
    }

    /// Project the full report to a `list`-shaped view: just the
    /// tasks (filtered by `source` when set) plus the schema version
    /// and root. Drops resolver state — `list` is purely a directory
    /// listing for tasks.
    pub(crate) fn into_list_view(self, source: Option<TaskSource>) -> TaskListView<'a> {
        let tasks = self
            .tasks
            .into_iter()
            .filter(|t| source.is_none_or(|s| s.label() == t.source))
            .collect();
        TaskListView {
            schema_version: self.schema_version,
            root: self.root,
            tasks,
        }
    }
}

/// `list --json` projection. Same `schema_version` as [`Project`] so
/// consumers can branch on it.
#[derive(Debug, Serialize)]
pub(crate) struct TaskListView<'a> {
    /// Identical to [`Project::schema_version`]; consumers can assume
    /// `1` here means a v1-shaped `tasks` array.
    pub schema_version: u32,
    /// Project root.
    pub root: String,
    /// Tasks, optionally filtered by source.
    pub tasks: Vec<TaskInfo<'a>>,
}

/// Detection results — what the file scan found, before any resolver
/// policy was applied.
#[derive(Debug, Serialize)]
pub(crate) struct Detected<'a> {
    /// Detected package managers, in detection-priority order.
    pub package_managers: Vec<&'static str>,
    /// Detected task runners.
    pub task_runners: Vec<&'static str>,
    /// `.nvmrc` / `.node-version` / `engines.node` declaration.
    pub node_version: Option<NodeVersionInfo<'a>>,
    /// `node --version` output, when the binary is on PATH.
    pub current_node: Option<&'a str>,
    /// Whether the project looks like a monorepo (workspace globs).
    pub monorepo: bool,
}

impl<'a> Detected<'a> {
    fn from_ctx(ctx: &'a ProjectContext) -> Self {
        Self {
            package_managers: ctx.package_managers.iter().map(|pm| pm.label()).collect(),
            task_runners: ctx.task_runners.iter().map(|tr| tr.label()).collect(),
            node_version: ctx.node_version.as_ref().map(|nv| NodeVersionInfo {
                expected: &nv.expected,
                source: nv.source,
            }),
            current_node: ctx.current_node.as_deref(),
            monorepo: ctx.is_monorepo,
        }
    }
}

/// Node version declaration plus the file it came from.
#[derive(Debug, Serialize)]
pub(crate) struct NodeVersionInfo<'a> {
    /// Version string as written (e.g. `"20.11.0"`, `">=18"`).
    pub expected: &'a str,
    /// Source file that declared the version (e.g. `".nvmrc"`).
    pub source: &'static str,
}

/// Materialised override stack — the inputs that fed into resolver
/// decisions.
#[derive(Debug, Serialize)]
pub(crate) struct OverridesView {
    /// Cross-ecosystem PM override from `--pm` / `RUNNER_PM`.
    pub pm: Option<PmOverrideInfo>,
    /// Per-ecosystem PM overrides from `runner.toml [pm].<eco>`.
    pub pm_by_ecosystem: BTreeMap<String, PmOverrideInfo>,
    /// `--runner` / `RUNNER_RUNNER` override.
    pub runner: Option<RunnerOverrideInfo>,
    /// Ranked preference list from `[task_runner].prefer`.
    pub prefer_runners: Vec<&'static str>,
    /// Active `FallbackPolicy` label.
    pub fallback: &'static str,
    /// Active `MismatchPolicy` label.
    pub on_mismatch: &'static str,
    /// Whether the explain trace is on.
    pub explain: bool,
    /// Whether warnings are suppressed.
    pub no_warnings: bool,
}

impl OverridesView {
    fn from_resolution_overrides(overrides: &ResolutionOverrides) -> Self {
        let mut pm_by_eco = BTreeMap::new();
        for (eco, pm_override) in &overrides.pm_by_ecosystem {
            pm_by_eco.insert(
                eco.label().to_string(),
                PmOverrideInfo {
                    pm: pm_override.pm.label(),
                    origin: origin_label(&pm_override.origin),
                },
            );
        }
        Self {
            pm: overrides.pm.as_ref().map(|o| PmOverrideInfo {
                pm: o.pm.label(),
                origin: origin_label(&o.origin),
            }),
            pm_by_ecosystem: pm_by_eco,
            runner: overrides.runner.as_ref().map(|o| RunnerOverrideInfo {
                runner: o.runner.label(),
                origin: origin_label(&o.origin),
            }),
            prefer_runners: overrides.prefer_runners.iter().map(|r| r.label()).collect(),
            fallback: fallback_label(overrides.fallback),
            on_mismatch: mismatch_label(overrides.on_mismatch),
            explain: overrides.explain,
            no_warnings: overrides.no_warnings,
        }
    }
}

/// PM override + provenance.
#[derive(Debug, Serialize)]
pub(crate) struct PmOverrideInfo {
    /// The chosen PM label.
    pub pm: &'static str,
    /// `"cli"`, `"env"`, or `"config:/abs/path"`.
    pub origin: String,
}

/// Task-runner override + provenance.
#[derive(Debug, Serialize)]
pub(crate) struct RunnerOverrideInfo {
    /// The chosen runner label.
    pub runner: &'static str,
    /// `"cli"`, `"env"`, or `"config:/abs/path"`.
    pub origin: String,
}

/// Per-ecosystem signals — what the resolver had to work with.
#[derive(Debug, Serialize)]
pub(crate) struct Signals {
    /// Node-ecosystem signals. The schema is intentionally
    /// node-flat today; other ecosystems get peer fields as their
    /// resolver paths land.
    pub node: NodeSignals,
}

/// Node-ecosystem detection signals: lockfile, manifest, PATH probe.
#[derive(Debug, Serialize)]
pub(crate) struct NodeSignals {
    /// PM inferred from the highest-priority lockfile, if any.
    pub lockfile_pm: Option<&'static str>,
    /// Manifest declaration (legacy `packageManager` or `devEngines`).
    pub manifest_pm: Option<ManifestPm>,
    /// `bun`/`pnpm`/`yarn`/`npm` -> absolute path on `$PATH` (or null).
    pub path_probe: BTreeMap<&'static str, Option<String>>,
}

/// Manifest-level PM declaration plus the field it came from.
#[derive(Debug, Serialize)]
pub(crate) struct ManifestPm {
    /// Declared PM label.
    pub pm: &'static str,
    /// Either `"packageManager"` or `"devEngines.packageManager"`.
    pub source: &'static str,
    /// Version constraint as written, if present.
    pub version: Option<String>,
    /// Effective `onFail` policy (`"ignore"`, `"warn"`, `"error"`).
    pub on_fail: &'static str,
}

/// Resolver verdict surface. Mirrors the resolver's `Result` so
/// consumers can branch on the variant before reading the inner shape.
#[derive(Debug, Serialize)]
pub(crate) struct Decisions {
    /// Node script-dispatch PM decision, or an error message when the
    /// resolver bailed.
    pub node_pm: NodePmDecision,
}

/// Either a resolved Node PM or the diagnostic string for the failure
/// that prevented one. Untagged so consumers can probe via "is the
/// `pm` field present?".
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum NodePmDecision {
    /// Successful resolution.
    Resolved {
        /// The chosen PM label.
        pm: &'static str,
        /// Human-readable `via` line — the same string `--explain` prints.
        via: String,
    },
    /// Resolver bailed; carries the rendered error message.
    Error {
        /// One-line error description from `ResolveError::Display`.
        error: String,
    },
}

/// Task entry projected into the JSON shape.
#[derive(Debug, Serialize)]
pub(crate) struct TaskInfo<'a> {
    /// Task name as it appears in the config.
    pub name: &'a str,
    /// Source label (e.g. `"package.json"`, `"justfile"`).
    pub source: &'static str,
    /// Human-readable description, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    /// When the task is an alias, the target it resolves to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<&'a str>,
    /// When the task's body is a thin wrapper for another runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passthrough_to: Option<&'static str>,
}

/// Warning projected into the JSON shape. The `source`/`detail` split
/// is kept stable from the pre-A4 flat-struct days so existing
/// consumers (the `doctor` test suite, ad-hoc `jq` queries) keep
/// working.
#[derive(Debug, Serialize)]
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

fn decisions_for(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> (Decisions, Vec<DetectionWarning>) {
    match Resolver::new(ctx, overrides).resolve_node_pm() {
        Ok(decision) => {
            let warnings = decision.warnings.clone();
            (
                Decisions {
                    node_pm: NodePmDecision::Resolved {
                        pm: decision.pm.label(),
                        via: decision.describe(),
                    },
                },
                warnings,
            )
        }
        Err(err) => (
            Decisions {
                node_pm: NodePmDecision::Error {
                    error: format!("{err}"),
                },
            },
            Vec::new(),
        ),
    }
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

const fn mismatch_label(policy: MismatchPolicy) -> &'static str {
    match policy {
        MismatchPolicy::Warn => "warn",
        MismatchPolicy::Error => "error",
        MismatchPolicy::Ignore => "ignore",
    }
}

/// Probe each Node PM in canonical order and report (binary, path) pairs.
/// Used by the doctor signals section; intentionally calls the real probe
/// so the output reflects what the resolver would see.
///
/// Probes run in parallel via [`std::thread::scope`]: each `probe_path_for_doctor`
/// call walks the entire `PATH` searching for one binary, which is O(N
/// entries) of independent `stat` syscalls. Doctor isn't on the hot
/// path, but four-way fan-out is essentially free and keeps the
/// rendering snappy on `PATH`s that contain network-mounted directories.
const PATH_PROBE_PMS: [PackageManager; 4] = [
    PackageManager::Bun,
    PackageManager::Pnpm,
    PackageManager::Yarn,
    PackageManager::Npm,
];

fn path_probe_map() -> BTreeMap<&'static str, Option<String>> {
    use std::env;
    use std::thread;

    let path = env::var_os("PATH").unwrap_or_default();
    let pathext = env::var_os("PATHEXT");
    let pathext_ref = pathext.as_deref();

    thread::scope(|s| {
        // Spawn all probes first (push, don't lazy-iterate) so they
        // actually run in parallel; chaining `.map(spawn).map(join)`
        // without the eager push would serialize — `Iterator::map` is
        // lazy, so the next `spawn` wouldn't fire until the previous
        // join returned.
        let mut handles = Vec::with_capacity(PATH_PROBE_PMS.len());
        for pm in &PATH_PROBE_PMS {
            let path = &path;
            handles.push(s.spawn(move || {
                let resolved =
                    crate::resolver::probe_path_for_doctor(pm.label(), path, pathext_ref)
                        .map(|p| p.display().to_string());
                (pm.label(), resolved)
            }));
        }

        let mut map = BTreeMap::new();
        for handle in handles {
            let (label, resolved) = handle.join().expect("path probe thread panicked");
            map.insert(label, resolved);
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Project;
    use crate::resolver::ResolutionOverrides;
    use crate::types::{PackageManager, ProjectContext};

    fn empty_context(root: &str) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from(root),
            package_managers: vec![PackageManager::Pnpm],
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn project_serializes_schema_version_field() {
        let ctx = empty_context("/tmp/test");
        let overrides = ResolutionOverrides::default();
        let project = Project::build(&ctx, &overrides);
        let value = serde_json::to_value(&project).expect("Project should serialize to JSON");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["root"], "/tmp/test");
        assert!(
            value["ecosystems"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
    }

    #[test]
    fn info_view_drops_tasks_array() {
        let mut ctx = empty_context("/tmp/test");
        ctx.tasks.push(crate::types::Task {
            name: "build".to_string(),
            source: crate::types::TaskSource::PackageJson,
            description: None,
            alias_of: None,
            passthrough_to: None,
        });
        let project = Project::build(&ctx, &ResolutionOverrides::default()).into_info_view();
        let value = serde_json::to_value(&project).expect("info view should serialize");

        // `skip_serializing_if = Vec::is_empty` collapses to no field.
        assert!(value.get("tasks").is_none(), "info view should omit tasks");
    }

    #[test]
    fn list_view_filters_by_source() {
        let mut ctx = empty_context("/tmp/test");
        ctx.tasks.push(crate::types::Task {
            name: "build".to_string(),
            source: crate::types::TaskSource::PackageJson,
            description: None,
            alias_of: None,
            passthrough_to: None,
        });
        ctx.tasks.push(crate::types::Task {
            name: "fmt".to_string(),
            source: crate::types::TaskSource::Justfile,
            description: None,
            alias_of: None,
            passthrough_to: None,
        });
        let project = Project::build(&ctx, &ResolutionOverrides::default());
        let view = project.into_list_view(Some(crate::types::TaskSource::Justfile));

        assert_eq!(view.tasks.len(), 1);
        assert_eq!(view.tasks[0].name, "fmt");
    }
}
