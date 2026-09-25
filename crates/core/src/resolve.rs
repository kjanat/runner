//! One resolver, every ecosystem, one override chain.

use std::collections::BTreeMap;

use crate::capability::BinDirs;
use crate::evidence::{Evidence, Present, Weight};
use crate::policy::{Choice, Policy};
use crate::provider::{Ecosystem, Kind, ProviderId};
use crate::registry::Registry;
use crate::scope::Scope;
use crate::task::Task;
use crate::tree::Tree;
use crate::warning::Warning;

/// The resolved project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Project {
    /// Every present provider.
    pub present: Vec<Present>,
    /// Every task, with its scope.
    pub tasks: Vec<Task>,
    /// Findings from observation and resolution.
    pub warnings: Vec<Warning>,
    /// Task sources whose tasks could not be read.
    pub unread: Vec<Unread>,
}

/// A task source whose read failed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unread {
    /// The task source.
    pub provider: ProviderId,
    /// The scope it was read in.
    pub scope: Scope,
    /// The failure.
    pub message: String,
}

impl Project {
    /// The provider observed in this scope, inheriting root evidence when absent.
    #[must_use]
    pub fn present_in(&self, id: ProviderId, scope: &Scope) -> Option<&Present> {
        self.present
            .iter()
            .find(|p| p.provider == id && &p.scope == scope)
            .or_else(|| {
                self.present
                    .iter()
                    .find(|p| p.provider == id && p.scope == Scope::Root)
            })
    }

    /// The package manager that accepts a source in this scope.
    #[must_use]
    pub fn for_source(
        &self,
        source: ProviderId,
        scope: &Scope,
        policy: &Policy,
        registry: &Registry,
    ) -> Option<&Present> {
        self.present
            .iter()
            .filter(|present| {
                self.present_in(present.provider, scope)
                    .is_some_and(|selected| std::ptr::eq(selected, *present))
                    && registry
                        .by_id(present.provider)
                        .kind
                        .contains(Kind::PACKAGE_MANAGER)
                    && registry
                        .by_id(present.provider)
                        .for_present(present)
                        .caps
                        .run_task
                        .is_some_and(|cap| cap.sources.contains(&source))
            })
            .min_by_key(|present| {
                (
                    !policy
                        .pm
                        .0
                        .values()
                        .any(|choice| choice.id == present.provider),
                    present.scope != *scope,
                    self.present.iter().position(|p| std::ptr::eq(p, *present)),
                )
            })
    }

    /// Query every present provider's executable directories again.
    ///
    /// A failed query leaves that provider without directories and adds a warning.
    pub fn refresh_bins(&mut self, tree: &Tree, registry: &Registry) {
        for present in &mut self.present {
            present.bin_dirs = match bin_dirs(tree, registry, present) {
                Ok(dirs) => dirs,
                Err(warning) => {
                    self.warnings.push(warning);
                    Vec::new()
                }
            };
        }
    }

    /// The present providers of `ecosystem` with `kind`, strongest evidence first.
    #[must_use]
    pub fn of(&self, ecosystem: Ecosystem, kind: Kind, registry: &Registry) -> Vec<&Present> {
        self.present
            .iter()
            .filter(|present| {
                let provider = registry.by_id(present.provider);
                provider.ecosystem == ecosystem && provider.kind.intersects(kind)
            })
            .collect()
    }
}

/// Resolve observed providers and extract their tasks.
///
/// # Errors
/// Returns observation failures.
pub fn resolve(
    tree: &Tree,
    evidence: Vec<Evidence>,
    policy: &Policy,
    registry: &Registry,
) -> std::io::Result<Project> {
    let mut project = resolve_presence(tree, evidence, policy, registry)?;
    let mut unread = Vec::new();
    for present in &project.present {
        if let Some(extract) = registry.by_id(present.provider).tasks {
            match extract(present, tree) {
                Ok(found) => {
                    project.warnings.extend(found.warnings);
                    for task in found.tasks {
                        if !project.tasks.iter().any(|existing| {
                            existing.source == task.source
                                && existing.scope == task.scope
                                && existing.name == task.name
                        }) {
                            project.tasks.push(task);
                        }
                    }
                }
                Err(warning) => unread.push(Unread {
                    provider: present.provider,
                    scope: present.scope.clone(),
                    message: warning.message,
                }),
            }
        }
    }
    project.unread = unread;
    Ok(project)
}

/// Turn evidence into present providers.
///
/// A provider is present in a scope when a signal stronger than a `PATH`
/// probe was found there, or when policy names it and it is on `PATH`.
/// Within an ecosystem the order is the policy's choice first, then by the
/// strongest evidence, the same rule for every ecosystem. Unless policy is
/// strict, a task source no present package manager can run gets the first
/// one on `PATH`, in `probe_priority` order.
///
/// # Errors
/// Returns observation failures.
pub fn resolve_presence(
    tree: &Tree,
    evidence: Vec<Evidence>,
    policy: &Policy,
    registry: &Registry,
) -> std::io::Result<Project> {
    let mut warnings = Vec::new();
    let mut present = observed_presence(evidence, policy, registry, &mut warnings);
    for choice in choices(policy) {
        if !present.iter().any(|p| p.provider == choice.id) {
            warnings.push(Warning::about(
                choice.id,
                format!(
                    "{} was chosen by {:?} but nothing shows it here",
                    registry.by_id(choice.id).label,
                    choice.from
                ),
            ));
        }
    }
    present.sort_by_key(|p| {
        let provider = registry.by_id(p.provider);
        (
            p.scope.clone(),
            provider.ecosystem,
            chosen_by(policy, p.provider).is_none(),
            p.because.first().map_or(Weight::Probed, |e| e.weight),
            provider.id,
        )
    });
    let mut project = Project {
        present,
        warnings,
        ..Project::default()
    };
    project.refresh_bins(tree, registry);
    add_task_runners(tree, policy, registry, &mut project);
    Ok(project)
}

/// Present providers grouped from evidence, before ordering.
fn observed_presence(
    evidence: Vec<Evidence>,
    policy: &Policy,
    registry: &Registry,
    warnings: &mut Vec<Warning>,
) -> Vec<Present> {
    let mut by_key: BTreeMap<(ProviderId, Scope), Vec<Evidence>> = BTreeMap::new();
    for item in evidence {
        let Some(provider) = item.provider else {
            continue;
        };
        by_key
            .entry((provider, item.scope.clone()))
            .or_default()
            .push(item);
    }
    let mut present = Vec::new();
    for ((id, scope), mut because) in by_key {
        let provider = registry.by_id(id);
        because.retain(|item| !matches!(item.declared, Some(crate::Declared::Alternative(other)) if other != id));
        let has_program = provider
            .program
            .is_some_and(|program| crate::probe::probe_with(program, &[]).is_some());
        because.retain(|item| {
            !matches!(
                item.signal.and_then(|id| provider.signals.get(id.0)),
                Some(crate::Signal::EnvVar(_))
            ) || has_program
        });
        because.sort_by_key(|item| item.weight);
        let Some(strongest) = because.first().map(|item| item.weight) else {
            continue;
        };
        if strongest == Weight::Probed && chosen_by(policy, id).is_none() {
            continue;
        }
        let mut observed = Present {
            provider: id,
            scope,
            version: None,
            bin_dirs: Vec::new(),
            because,
        };
        if let Some(version) = provider.version {
            match version(&observed) {
                Ok(value) => observed.version = Some(value),
                Err(warning) => warnings.push(warning),
            }
        }
        present.push(observed);
    }
    present
}

/// Give each task source no present package manager runs the first one on `PATH`.
fn add_task_runners(tree: &Tree, policy: &Policy, registry: &Registry, project: &mut Project) {
    let sources: Vec<_> = project
        .present
        .iter()
        .filter(|present| {
            registry
                .by_id(present.provider)
                .kind
                .contains(Kind::TASK_SOURCE)
        })
        .map(|present| (present.provider, present.scope.clone()))
        .collect();
    for (source, scope) in sources {
        if project
            .for_source(source, &scope, policy, registry)
            .is_some()
        {
            continue;
        }
        let supports = |provider: &crate::Provider| {
            provider.kind.contains(Kind::PACKAGE_MANAGER)
                && provider
                    .caps
                    .run_task
                    .is_some_and(|cap| cap.sources.contains(&source))
        };
        if policy.strict
            || policy
                .pm
                .0
                .values()
                .any(|choice| supports(registry.by_id(choice.id)))
        {
            continue;
        }
        let bins: Vec<_> = project
            .present
            .iter()
            .filter(|present| present.scope == scope || present.scope == Scope::Root)
            .flat_map(|present| present.bin_dirs.iter().cloned())
            .collect();
        let mut candidates: Vec<_> = registry.iter().filter(|p| supports(p)).collect();
        candidates.sort_by_key(|provider| provider.caps.probe_priority);
        let Some((provider, at, signal)) = candidates.into_iter().find_map(|provider| {
            let program = provider.program?;
            let at = crate::probe::probe_with(program, &bins)?;
            let signal = provider.signals.iter().position(
                |signal| matches!(signal, crate::Signal::Probe(name) if *name == program),
            )?;
            Some((provider, at, signal))
        }) else {
            continue;
        };
        let mut synthesised = Present {
            provider: provider.id,
            scope: scope.clone(),
            version: None,
            bin_dirs: Vec::new(),
            because: vec![Evidence {
                provider: Some(provider.id),
                signal: Some(crate::SignalId(signal)),
                at,
                scope,
                weight: Weight::Probed,
                declared: None,
            }],
        };
        match bin_dirs(tree, registry, &synthesised) {
            Ok(dirs) => synthesised.bin_dirs = dirs,
            Err(warning) => project.warnings.push(warning),
        }
        project.present.push(synthesised);
    }
}

/// The executable directories `present` declares.
fn bin_dirs(
    tree: &Tree,
    registry: &Registry,
    present: &Present,
) -> Result<Vec<std::path::PathBuf>, Warning> {
    let provider = registry.by_id(present.provider).for_present(present);
    let dir = crate::plan::scope_dir(tree, &present.scope);
    match provider.caps.bins.map(|cap| cap.dirs) {
        Some(BinDirs::Static(dirs)) => Ok(dirs.iter().map(|bin| dir.join(bin)).collect()),
        Some(BinDirs::Ask(ask)) => ask(&dir).map_err(|error| {
            Warning::about(
                provider.id,
                format!(
                    "{} executable directories in {}: {error}",
                    provider.label,
                    dir.display()
                ),
            )
        }),
        None => Ok(Vec::new()),
    }
}

fn choices(policy: &Policy) -> impl Iterator<Item = &Choice> {
    policy
        .pm
        .0
        .values()
        .chain(policy.runner.iter())
        .chain(policy.runtime.iter())
}

fn chosen_by(policy: &Policy, id: ProviderId) -> Option<&Choice> {
    choices(policy).find(|choice| choice.id == id)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::resolve;
    use crate::capability::{BinDirs, BinsCap, Capabilities};
    use crate::evidence::{Evidence, Weight};
    use crate::policy::{Choice, Layer, Policy};
    use crate::provider::{Ecosystem, Hooks, Kind, ProviderId};
    use crate::registry::{Provider, Registry};
    use crate::scope::Scope;
    use crate::signal::{Signal, SignalId};
    use crate::tree::Tree;

    const fn fake(id: ProviderId, label: &'static str, ecosystem: Ecosystem) -> Provider {
        Provider {
            id,
            label,
            aliases: &[],
            ecosystem,
            kind: Kind::PACKAGE_MANAGER,
            program: Some(label),
            signals: &[Signal::Probe("x")],
            writes: &[],
            caps: Capabilities {
                bins: Some(BinsCap {
                    dirs: BinDirs::Static(&["node_modules/.bin"]),
                }),
                ..Capabilities::NONE
            },
            tasks: None,
            version: None,
            hooks: Hooks::NONE,
        }
    }

    static FAKES: &[Provider] = &[
        fake(ProviderId::Npm, "npm", Ecosystem::Node),
        fake(ProviderId::Pnpm, "pnpm", Ecosystem::Node),
        fake(ProviderId::Uv, "uv", Ecosystem::Python),
    ];

    fn tree() -> Tree {
        Tree {
            cwd: PathBuf::from("/p"),
            root: PathBuf::from("/p"),
            members: Vec::new(),
        }
    }

    fn found(provider: ProviderId, weight: Weight) -> Evidence {
        Evidence {
            provider: Some(provider),
            signal: Some(SignalId(0)),
            at: PathBuf::from("/p/x"),
            scope: Scope::Root,
            weight,
            declared: None,
        }
    }

    #[test]
    fn every_ecosystem_goes_through_the_same_rule() {
        let registry = Registry(FAKES);
        let evidence = vec![
            found(ProviderId::Npm, Weight::Probed),
            found(ProviderId::Pnpm, Weight::Locked),
            found(ProviderId::Uv, Weight::Locked),
        ];
        let project = resolve(&tree(), evidence, &Policy::default(), &registry).unwrap();
        let ids: Vec<ProviderId> = project.present.iter().map(|p| p.provider).collect();
        assert_eq!(ids, [ProviderId::Pnpm, ProviderId::Uv]);
        assert_eq!(
            project.present[0].bin_dirs,
            [PathBuf::from("/p/node_modules/.bin")]
        );
    }

    #[test]
    fn a_policy_choice_outranks_evidence_and_admits_a_probe() {
        let registry = Registry(FAKES);
        let mut policy = Policy::default();
        policy.pm.0.insert(
            Ecosystem::Node,
            Choice {
                id: ProviderId::Npm,
                from: Layer::Cli,
            },
        );
        let evidence = vec![
            found(ProviderId::Npm, Weight::Probed),
            found(ProviderId::Pnpm, Weight::Locked),
        ];
        let project = resolve(&tree(), evidence, &policy, &registry).unwrap();
        let ids: Vec<ProviderId> = project.present.iter().map(|p| p.provider).collect();
        assert_eq!(ids, [ProviderId::Npm, ProviderId::Pnpm]);
        assert!(project.warnings.is_empty());
        let absent = resolve(&tree(), Vec::new(), &policy, &registry).unwrap();
        assert_eq!(absent.warnings.len(), 1);
    }
}
