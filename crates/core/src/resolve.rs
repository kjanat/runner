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
}

impl Project {
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

/// Turn evidence into present providers and tasks.
///
/// A provider is present in a scope when a signal stronger than a `PATH`
/// probe was found there, or when policy names it and it is on `PATH`.
/// Within an ecosystem the order is the policy's choice first, then by the
/// strongest evidence, the same rule for every ecosystem.
#[must_use]
pub fn resolve(
    tree: &Tree,
    evidence: Vec<Evidence>,
    policy: &Policy,
    registry: &Registry,
) -> Project {
    let mut by_key: BTreeMap<(ProviderId, Scope), Vec<Evidence>> = BTreeMap::new();
    for item in evidence {
        by_key
            .entry((item.provider, item.scope.clone()))
            .or_default()
            .push(item);
    }
    let mut warnings = Vec::new();
    let mut present: Vec<Present> = Vec::new();
    for ((id, scope), mut because) in by_key {
        because.sort_by_key(|item| item.weight);
        let provider = registry.by_id(id);
        let chosen = chosen_by(policy, id).is_some();
        let strongest = because.first().map_or(Weight::Probed, |item| item.weight);
        if strongest == Weight::Probed && !chosen {
            continue;
        }
        let dir = match &scope {
            Scope::Root => tree.root.clone(),
            Scope::Member { dir, .. } => dir.clone(),
        };
        let bin_dirs = match provider.caps.bins.map(|bins| bins.dirs) {
            Some(BinDirs::Static(dirs)) => dirs.iter().map(|d| dir.join(d)).collect(),
            Some(BinDirs::Ask(ask)) => ask(&dir),
            None => Vec::new(),
        };
        present.push(Present {
            provider: id,
            scope,
            version: None,
            bin_dirs,
            because,
        });
    }
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
    present.sort_by(|a, b| {
        let rank = |p: &Present| {
            let provider = registry.by_id(p.provider);
            let chosen = chosen_by(policy, p.provider).map_or(1, |_| 0);
            let strongest = p.because.first().map_or(Weight::Probed, |e| e.weight);
            (
                p.scope.clone(),
                provider.ecosystem,
                chosen,
                strongest,
                provider.id,
            )
        };
        rank(a).cmp(&rank(b))
    });
    let mut tasks = Vec::new();
    for item in &present {
        if let Some(extract) = registry.by_id(item.provider).tasks {
            match extract(item, tree) {
                Ok(found) => tasks.extend(found),
                Err(warning) => warnings.push(warning),
            }
        }
    }
    Project {
        present,
        tasks,
        warnings,
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
            provider,
            signal: SignalId(0),
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
        let project = resolve(&tree(), evidence, &Policy::default(), &registry);
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
        let project = resolve(&tree(), evidence, &policy, &registry);
        let ids: Vec<ProviderId> = project.present.iter().map(|p| p.provider).collect();
        assert_eq!(ids, [ProviderId::Npm, ProviderId::Pnpm]);
        assert!(project.warnings.is_empty());
        let absent = resolve(&tree(), Vec::new(), &policy, &registry);
        assert_eq!(absent.warnings.len(), 1);
    }
}
