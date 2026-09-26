//! The package manager the core chose for a task source, as reports describe it.

use std::path::PathBuf;

use runner_core::{
    Declared, Evidence, Layer, OnFail, Plan, Policy, Present, Project, ProviderId, Scope, Signal,
    Tree, decided_by,
};
use runner_providers::REGISTRY;

use crate::resolver::{MismatchPolicy, ResolutionOverrides};
use crate::types::{DetectionWarning, PackageManager};

/// Observation and resolution for one invocation.
pub(crate) struct Observed {
    pub tree: Tree,
    pub policy: Policy,
    pub project: Project,
}

impl Observed {
    /// Observe and resolve `ctx` under the invocation's overrides.
    ///
    /// # Errors
    /// Returns observation failures.
    pub(crate) fn observe(
        ctx: &crate::types::ProjectContext,
        overrides: &ResolutionOverrides,
    ) -> std::io::Result<Self> {
        let policy = super::core::policy(overrides);
        let project = super::core::project_under(ctx, &policy)?;
        Ok(Self {
            tree: super::core::tree(ctx),
            policy,
            project,
        })
    }

    /// The package manager that dispatches `source` in the invocation scope.
    pub(crate) fn decision(&self, source: ProviderId) -> Option<PmDecision> {
        decide(&self.tree, &self.project, &self.policy, source)
    }

    /// What the manifest in the invocation scope declares for `source`'s package manager.
    pub(crate) fn manifest_declaration(&self, source: ProviderId) -> Option<ManifestDeclaration> {
        let scope = runner_core::plan::scope_at(&self.tree, &self.tree.cwd);
        self.project
            .present
            .iter()
            .filter(|present| dispatches(present.provider, source))
            .filter(|present| {
                self.project
                    .present_in(present.provider, &scope)
                    .is_some_and(|chosen| std::ptr::eq(chosen, *present))
            })
            .find_map(|present| {
                let pm = PackageManager::from_label(REGISTRY.by_id(present.provider).label)?;
                let (evidence, field) = manifest_field(present)?;
                Some(ManifestDeclaration {
                    pm,
                    field,
                    version: evidence
                        .declared
                        .as_ref()
                        .and_then(Declared::version)
                        .map(str::to_owned),
                    on_fail: match &evidence.declared {
                        Some(Declared::Constraint { on_fail, .. }) => *on_fail,
                        _ => OnFail::Ignore,
                    },
                })
            })
    }
}

/// A package-manager declaration read from a manifest.
pub(crate) struct ManifestDeclaration {
    pub pm: PackageManager,
    pub field: &'static str,
    pub version: Option<String>,
    pub on_fail: OnFail,
}

/// The package manager that dispatches `source` in the invocation scope.
pub(crate) fn decide(
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    source: ProviderId,
) -> Option<PmDecision> {
    decide_in(
        project,
        policy,
        source,
        &runner_core::plan::scope_at(tree, &tree.cwd),
    )
}

/// The package manager that dispatches `source` in `scope`.
pub(crate) fn decide_in(
    project: &Project,
    policy: &Policy,
    source: ProviderId,
    scope: &Scope,
) -> Option<PmDecision> {
    let present = project.for_source(source, scope, policy, &REGISTRY)?;
    PmDecision::new(
        present.provider,
        &decided_by(policy, present),
        &present.because,
        scope,
    )
}

/// A package-manager decision, from the layer that made it and the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PmDecision {
    pub pm: PackageManager,
    pub layer: Layer,
    pub at: PathBuf,
    pub field: Option<&'static str>,
    pub on_fail: Option<OnFail>,
    pub scope: Scope,
}

impl PmDecision {
    fn new(
        provider: ProviderId,
        decided_by: &[Layer],
        because: &[Evidence],
        scope: &Scope,
    ) -> Option<Self> {
        let pm = PackageManager::from_label(REGISTRY.by_id(provider).label)?;
        let layer = decided_by.first()?.clone();
        let strongest = because.first();
        let declared = strongest.and_then(|e| {
            let signals = REGISTRY.by_id(provider).signals;
            match signals.get(e.signal?.0) {
                Some(Signal::ManifestField { path, .. }) => Some((*path, e.declared.as_ref())),
                _ => None,
            }
        });
        Some(Self {
            pm,
            layer,
            at: strongest.map(|e| e.at.clone()).unwrap_or_default(),
            field: declared.map(|(field, _)| field),
            on_fail: declared.and_then(|(_, declared)| match declared {
                Some(Declared::Constraint { on_fail, .. }) => Some(*on_fail),
                _ => None,
            }),
            scope: scope.clone(),
        })
    }

    /// The decision a plan carries.
    pub(crate) fn from_plan(plan: &Plan) -> Option<Self> {
        Self::new(plan.provider?, &plan.decided_by, &plan.because, &plan.scope)
    }

    /// One line naming the package manager and the layer that chose it.
    pub(crate) fn describe(&self) -> String {
        let pm = self.pm.label();
        match &self.layer {
            Layer::Cli => format!("{pm} via --pm (CLI override)"),
            Layer::Env => format!("{pm} via RUNNER_PM (environment)"),
            Layer::ConfigFile(path) => format!("{pm} via runner.toml at {}", path.display()),
            Layer::Manifest(path) => {
                let field = self
                    .field
                    .map(|field| format!(" {field:?}"))
                    .unwrap_or_default();
                let on_fail = self
                    .on_fail
                    .map(|on_fail| format!(" (onFail={})", on_fail.label()))
                    .unwrap_or_default();
                format!("{pm} via {}{field}{on_fail}", file_name(path))
            }
            Layer::Lockfile(path) => format!("{pm} via {}", file_name(path)),
            Layer::Probe => format!("{pm} via PATH probe at {}", self.at.display()),
        }
    }

    /// Whether the decision rests on nothing stronger than the executable being on `PATH`.
    pub(crate) const fn probed(&self) -> bool {
        matches!(self.layer, Layer::Probe)
    }

    /// The findings the decision carries: a `PATH` fallback, and a manifest
    /// that disagrees with the lockfile unless the invocation ignores it.
    pub(crate) fn warnings(
        &self,
        project: &Project,
        overrides: &ResolutionOverrides,
    ) -> Vec<DetectionWarning> {
        let mut warnings = Vec::new();
        let provider = REGISTRY
            .by_label(self.pm.label())
            .map(|provider| provider.id);
        if self.probed() {
            warnings.push(DetectionWarning::PathProbeFallback {
                picked: self.pm,
                ecosystem: self.pm.ecosystem(),
                others_available: provider
                    .map(|id| {
                        probe_order_for(id)
                            .into_iter()
                            .filter(|other| *other != self.pm)
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }
        if overrides.on_mismatch == MismatchPolicy::Ignore {
            return warnings;
        }
        for disagreement in &project.disagreements {
            if Some(disagreement.declared) != provider || disagreement.scope != self.scope {
                continue;
            }
            let Some(lockfile) =
                PackageManager::from_label(REGISTRY.by_id(disagreement.locked).label)
            else {
                continue;
            };
            warnings.push(DetectionWarning::PmMismatch {
                declared: self.pm,
                field: self.field.unwrap_or("packageManager"),
                lockfile,
            });
        }
        warnings
    }
}

fn file_name(path: &std::path::Path) -> std::borrow::Cow<'_, str> {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
}

/// The package managers on `PATH` that dispatch what `provider` dispatches, in probe order.
fn probe_order_for(provider: ProviderId) -> Vec<PackageManager> {
    let Some(cap) = REGISTRY.by_id(provider).caps.run_task else {
        return Vec::new();
    };
    let mut providers: Vec<_> = REGISTRY
        .iter()
        .filter(|candidate| {
            candidate.kind.contains(runner_core::Kind::PACKAGE_MANAGER)
                && candidate.caps.run_task.is_some_and(|other| {
                    other
                        .sources
                        .iter()
                        .any(|source| cap.sources.contains(source))
                })
                && candidate
                    .program
                    .is_some_and(|program| runner_core::probe_with(program, &[]).is_some())
        })
        .collect();
    providers.sort_by_key(|candidate| candidate.caps.probe_priority);
    providers
        .into_iter()
        .filter_map(|candidate| PackageManager::from_label(candidate.label))
        .collect()
}

fn dispatches(provider: ProviderId, source: ProviderId) -> bool {
    REGISTRY
        .by_id(provider)
        .caps
        .run_task
        .is_some_and(|cap| cap.sources.contains(&source))
}

/// The manifest-field evidence behind `present`, strongest first.
fn manifest_field(present: &Present) -> Option<(&Evidence, &'static str)> {
    let signals = REGISTRY.by_id(present.provider).signals;
    present
        .because
        .iter()
        .find_map(|evidence| match signals.get(evidence.signal?.0) {
            Some(Signal::ManifestField { path, .. }) => Some((evidence, *path)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use runner_core::{Layer, OnFail, ProviderId, Scope};

    use super::{Observed, PmDecision};
    use crate::resolver::{MismatchPolicy, OverrideOrigin, PmOverride, ResolutionOverrides};
    use crate::tool::test_support::TempDir;
    use crate::types::{DetectionWarning, Ecosystem, PackageManager};

    fn project(name: &str, files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new(name);
        for (file, body) in files {
            std::fs::write(dir.path().join(file), body).expect("fixture file");
        }
        dir
    }

    fn node_decision(dir: &TempDir, overrides: &ResolutionOverrides) -> Option<PmDecision> {
        let ctx = crate::detect::detect(dir.path());
        Observed::observe(&ctx, overrides)
            .expect("observation")
            .decision(ProviderId::PackageJson)
    }

    fn node_warnings(dir: &TempDir, overrides: &ResolutionOverrides) -> Vec<DetectionWarning> {
        let ctx = crate::detect::detect(dir.path());
        let observed = Observed::observe(&ctx, overrides).expect("observation");
        observed
            .decision(ProviderId::PackageJson)
            .map(|decision| decision.warnings(&observed.project, overrides))
            .unwrap_or_default()
    }

    fn with_pm_override(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..ResolutionOverrides::default()
        }
    }

    fn with_config_pm(pm: PackageManager, eco: Ecosystem) -> ResolutionOverrides {
        let mut map = HashMap::new();
        map.insert(
            eco,
            PmOverride {
                pm,
                origin: OverrideOrigin::ConfigFile {
                    path: PathBuf::from("/test/runner.toml"),
                },
            },
        );
        ResolutionOverrides {
            pm_by_ecosystem: map,
            ..ResolutionOverrides::default()
        }
    }

    const LOCK: &str = "lockfileVersion: 9\n";

    #[test]
    fn resolves_detected_node_pm_via_lockfile() {
        let dir = project(
            "decision-lockfile",
            &[("package.json", "{}"), ("pnpm-lock.yaml", LOCK)],
        );
        let decision = node_decision(&dir, &ResolutionOverrides::default()).expect("pnpm");
        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert!(matches!(decision.layer, Layer::Lockfile(_)));
        let described = decision.describe();
        assert!(
            described.starts_with("pnpm via ") && described.ends_with("pnpm-lock.yaml"),
            "{described}"
        );
    }

    #[test]
    fn a_project_without_node_evidence_has_no_node_decision() {
        let dir = project("decision-go", &[("go.mod", "module example.com/x\n")]);
        assert!(node_decision(&dir, &ResolutionOverrides::default()).is_none());
    }

    #[test]
    fn prefers_node_pm_over_non_node_primary() {
        let dir = project(
            "decision-node-over-cargo",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"x\"\nversion = \"0.0.0\"\n",
                ),
                ("package.json", "{}"),
                ("bun.lock", ""),
            ],
        );
        let decision = node_decision(&dir, &ResolutionOverrides::default()).expect("bun");
        assert_eq!(decision.pm, PackageManager::Bun);
    }

    #[test]
    fn deno_dispatches_package_json_scripts_when_no_node_pm_is_present() {
        let dir = project(
            "decision-deno",
            &[
                ("package.json", "{}"),
                ("deno.json", "{}"),
                ("deno.lock", "{}"),
            ],
        );
        let decision = node_decision(&dir, &ResolutionOverrides::default()).expect("deno");
        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn cli_and_env_overrides_beat_a_detected_pm() {
        let dir = project(
            "decision-override",
            &[
                ("package.json", "{}"),
                ("pnpm-lock.yaml", LOCK),
                ("yarn.lock", ""),
            ],
        );
        let cli = node_decision(
            &dir,
            &with_pm_override(PackageManager::Yarn, OverrideOrigin::CliFlag),
        )
        .expect("yarn");
        assert_eq!(cli.pm, PackageManager::Yarn);
        assert_eq!(cli.describe(), "yarn via --pm (CLI override)");
        let env = node_decision(
            &dir,
            &with_pm_override(PackageManager::Yarn, OverrideOrigin::EnvVar),
        )
        .expect("yarn");
        assert_eq!(env.describe(), "yarn via RUNNER_PM (environment)");
    }

    #[test]
    fn pm_override_for_deno_is_honored_for_node_scripts() {
        let dir = project(
            "decision-deno-override",
            &[
                ("package.json", "{}"),
                ("pnpm-lock.yaml", LOCK),
                ("deno.json", "{}"),
                ("deno.lock", "{}"),
            ],
        );
        let decision = node_decision(
            &dir,
            &with_pm_override(PackageManager::Deno, OverrideOrigin::CliFlag),
        )
        .expect("deno");
        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn manifest_package_manager_field_beats_lockfile_signal_and_warns() {
        let dir = project(
            "decision-manifest-wins",
            &[
                ("package.json", r#"{ "packageManager": "yarn@4.3.0" }"#),
                ("pnpm-lock.yaml", LOCK),
            ],
        );
        let overrides = ResolutionOverrides::default();
        let decision = node_decision(&dir, &overrides).expect("yarn");
        assert_eq!(decision.pm, PackageManager::Yarn);
        assert!(matches!(decision.layer, Layer::Manifest(_)));
        assert_eq!(decision.field, Some("packageManager"));
        let described = decision.describe();
        assert!(
            described.starts_with("yarn via ")
                && described.ends_with("package.json \"packageManager\""),
            "{described}"
        );
        let warnings = node_warnings(&dir, &overrides);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].source(), "package.json");
        assert!(warnings[0].detail().contains("declaration wins"));
    }

    #[test]
    fn a_declared_deno_outranks_npm_evidence() {
        for (name, files) in [
            (
                "decision-deno-over-lockfile",
                &[
                    (
                        "package.json",
                        r#"{ "packageManager": "deno@2.8.0", "scripts": { "build": "x" } }"#,
                    ),
                    ("package-lock.json", "{}"),
                ][..],
            ),
            (
                "decision-deno-over-dev-engines",
                &[(
                    "package.json",
                    r#"{ "packageManager": "deno@2.8.0", "devEngines": { "packageManager": { "name": "npm" } } }"#,
                )],
            ),
        ] {
            let dir = project(name, files);
            let ctx = crate::detect::detect(dir.path());
            let decision = Observed::observe(&ctx, &ResolutionOverrides::default())
                .expect("observation")
                .decision(ProviderId::PackageJson);
            assert_eq!(decision.map(|d| d.pm), Some(PackageManager::Deno), "{name}");
        }
    }

    #[test]
    fn dev_engines_used_when_package_manager_absent() {
        let dir = project(
            "decision-dev-engines",
            &[(
                "package.json",
                r#"{ "devEngines": { "packageManager": { "name": "bun", "onFail": "warn" } } }"#,
            )],
        );
        let decision = node_decision(&dir, &ResolutionOverrides::default()).expect("bun");
        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.field, Some("devEngines.packageManager"));
        assert_eq!(decision.on_fail, Some(OnFail::Warn));
        assert!(
            decision
                .describe()
                .ends_with("\"devEngines.packageManager\" (onFail=warn)")
        );
    }

    #[test]
    fn cli_override_still_beats_manifest_declaration() {
        let dir = project(
            "decision-cli-beats-manifest",
            &[
                ("package.json", r#"{ "packageManager": "yarn@4" }"#),
                ("bun.lock", ""),
            ],
        );
        let decision = node_decision(
            &dir,
            &with_pm_override(PackageManager::Bun, OverrideOrigin::CliFlag),
        )
        .expect("bun");
        assert_eq!(decision.pm, PackageManager::Bun);
        assert_eq!(decision.layer, Layer::Cli);
    }

    #[test]
    fn matching_lockfile_and_manifest_produce_no_warning() {
        let dir = project(
            "decision-matching",
            &[
                ("package.json", r#"{ "packageManager": "pnpm@9" }"#),
                ("pnpm-lock.yaml", LOCK),
            ],
        );
        let overrides = ResolutionOverrides::default();
        let decision = node_decision(&dir, &overrides).expect("pnpm");
        assert_eq!(decision.pm, PackageManager::Pnpm);
        assert!(matches!(decision.layer, Layer::Manifest(_)));
        assert_eq!(node_warnings(&dir, &overrides).len(), 0);
    }

    #[test]
    fn on_mismatch_ignore_drops_the_disagreement_warning() {
        let dir = project(
            "decision-mismatch-ignore",
            &[
                ("package.json", r#"{ "packageManager": "yarn@4" }"#),
                ("pnpm-lock.yaml", LOCK),
            ],
        );
        let ignore = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Ignore,
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            node_decision(&dir, &ignore).map(|d| d.pm),
            Some(PackageManager::Yarn)
        );
        assert_eq!(node_warnings(&dir, &ignore).len(), 0);
        let warn = ResolutionOverrides {
            on_mismatch: MismatchPolicy::Warn,
            ..ResolutionOverrides::default()
        };
        assert_eq!(node_warnings(&dir, &warn).len(), 1);
    }

    #[test]
    fn config_pm_node_field_overrides_detection() {
        let dir = project(
            "decision-config-node",
            &[
                ("package.json", "{}"),
                ("pnpm-lock.yaml", LOCK),
                ("yarn.lock", ""),
            ],
        );
        let decision = node_decision(&dir, &with_config_pm(PackageManager::Yarn, Ecosystem::Node))
            .expect("yarn");
        assert_eq!(decision.pm, PackageManager::Yarn);
        assert_eq!(
            decision.describe(),
            "yarn via runner.toml at /test/runner.toml"
        );
        let mut both = with_config_pm(PackageManager::Yarn, Ecosystem::Node);
        both.pm = Some(PmOverride {
            pm: PackageManager::Pnpm,
            origin: OverrideOrigin::CliFlag,
        });
        assert_eq!(
            node_decision(&dir, &both).map(|d| d.pm),
            Some(PackageManager::Pnpm)
        );
    }

    #[test]
    fn deno_config_value_fills_the_node_slot_and_resolves_for_node_scripts() {
        let dir = project(
            "decision-config-deno",
            &[
                ("package.json", "{}"),
                ("pnpm-lock.yaml", LOCK),
                ("deno.json", "{}"),
                ("deno.lock", "{}"),
            ],
        );
        let decision = node_decision(&dir, &with_config_pm(PackageManager::Deno, Ecosystem::Node))
            .expect("deno");
        assert_eq!(decision.pm, PackageManager::Deno);
    }

    #[test]
    fn a_manifest_declaration_is_reported_with_its_field_version_and_on_fail() {
        let dir = project(
            "decision-manifest-declaration",
            &[(
                "package.json",
                r#"{ "devEngines": { "packageManager": { "name": "pnpm", "version": "9.0.0" } } }"#,
            )],
        );
        let ctx = crate::detect::detect(dir.path());
        let observed =
            Observed::observe(&ctx, &ResolutionOverrides::default()).expect("observation");
        let declaration = observed
            .manifest_declaration(ProviderId::PackageJson)
            .expect("declared");
        assert_eq!(declaration.pm, PackageManager::Pnpm);
        assert_eq!(declaration.field, "devEngines.packageManager");
        assert_eq!(declaration.version.as_deref(), Some("9.0.0"));
        assert_eq!(declaration.on_fail, OnFail::Error);
        let legacy = project(
            "decision-manifest-declaration-legacy",
            &[("package.json", r#"{ "packageManager": "yarn@4.3.0" }"#)],
        );
        let ctx = crate::detect::detect(legacy.path());
        let declaration = Observed::observe(&ctx, &ResolutionOverrides::default())
            .expect("observation")
            .manifest_declaration(ProviderId::PackageJson)
            .expect("declared");
        assert_eq!(declaration.pm, PackageManager::Yarn);
        assert_eq!(declaration.field, "packageManager");
        assert_eq!(declaration.version.as_deref(), Some("4.3.0"));
        assert_eq!(declaration.on_fail, OnFail::Ignore);
    }

    #[test]
    fn a_probe_decision_reports_the_path_fallback() {
        let dir = project("decision-probe", &[("package.json", "{}")]);
        let overrides = ResolutionOverrides::default();
        let Some(decision) = node_decision(&dir, &overrides) else {
            return;
        };
        assert!(decision.probed(), "{decision:?}");
        assert!(decision.describe().contains("via PATH probe at "));
        let warnings = node_warnings(&dir, &overrides);
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, DetectionWarning::PathProbeFallback { picked, .. } if *picked == decision.pm)),
            "{warnings:?}"
        );
    }

    #[test]
    fn describe_renders_every_layer_by_name() {
        let decision = |pm, layer, field, on_fail| PmDecision {
            pm,
            layer,
            at: PathBuf::from("/usr/bin/npm"),
            field,
            on_fail,
            scope: Scope::Root,
        };
        let cases = [
            (
                decision(PackageManager::Yarn, Layer::Cli, None, None),
                "yarn via --pm (CLI override)",
            ),
            (
                decision(PackageManager::Bun, Layer::Env, None, None),
                "bun via RUNNER_PM (environment)",
            ),
            (
                decision(
                    PackageManager::Pnpm,
                    Layer::ConfigFile(PathBuf::from("/proj/runner.toml")),
                    None,
                    None,
                ),
                "pnpm via runner.toml at /proj/runner.toml",
            ),
            (
                decision(
                    PackageManager::Pnpm,
                    Layer::Manifest(PathBuf::from("/proj/package.json")),
                    Some("packageManager"),
                    None,
                ),
                "pnpm via package.json \"packageManager\"",
            ),
            (
                decision(
                    PackageManager::Bun,
                    Layer::Manifest(PathBuf::from("/proj/package.json")),
                    Some("devEngines.packageManager"),
                    Some(OnFail::Error),
                ),
                "bun via package.json \"devEngines.packageManager\" (onFail=error)",
            ),
            (
                decision(
                    PackageManager::Pnpm,
                    Layer::Lockfile(PathBuf::from("/proj/pnpm-lock.yaml")),
                    None,
                    None,
                ),
                "pnpm via pnpm-lock.yaml",
            ),
            (
                decision(PackageManager::Npm, Layer::Probe, None, None),
                "npm via PATH probe at /usr/bin/npm",
            ),
        ];
        for (decision, expected) in cases {
            assert_eq!(decision.describe(), expected);
        }
    }
}
