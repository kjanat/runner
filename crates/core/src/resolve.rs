//! One resolver, every ecosystem, one override chain.

use std::collections::BTreeMap;

use crate::capability::BinDirs;
use crate::evidence::{Evidence, Present, Weight};
use crate::policy::{Choice, Policy};
use crate::provider::{Ecosystem, Kind, ProviderId};
use crate::registry::{Provider, Registry};
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
    /// Manifests and lockfiles that name different package managers.
    pub disagreements: Vec<Disagreement>,
}

/// A manifest and a lockfile in one scope that name different package managers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Disagreement {
    /// The scope both were found in.
    pub scope: Scope,
    /// The provider the manifest declares.
    pub declared: ProviderId,
    /// The manifest.
    pub manifest: std::path::PathBuf,
    /// The provider the lockfile pins.
    pub locked: ProviderId,
    /// The lockfile.
    pub lockfile: std::path::PathBuf,
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
                    present
                        .because
                        .first()
                        .map_or((Weight::Probed, 0), Evidence::strength),
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
    let jobs: Vec<_> = project
        .present
        .iter()
        .filter_map(|present| {
            registry
                .by_id(present.provider)
                .tasks
                .map(|extract| (present, extract))
        })
        .collect();
    let extracted = extract_all(tree, &jobs);
    let mut unread = Vec::new();
    for ((present, _), outcome) in jobs.iter().zip(extracted) {
        let (provider, scope) = (present.provider, present.scope.clone());
        match outcome {
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
                provider,
                scope,
                message: warning.message,
            }),
        }
    }
    project.unread = unread;
    Ok(project)
}

type Extract = fn(&Present, &Tree) -> Result<crate::Extracted, Warning>;

/// Run every extractor on at most as many threads as the host runs at once,
/// returning the outcomes in `jobs` order. A job no thread could be spawned
/// for runs on the calling thread.
fn extract_all(
    tree: &Tree,
    jobs: &[(&Present, Extract)],
) -> Vec<Result<crate::Extracted, Warning>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some((present, extract)) = jobs.get(index) else {
                return done;
            };
            done.push((index, extract(present, tree)));
        }
    };
    let workers = std::thread::available_parallelism()
        .map_or(2, std::num::NonZero::get)
        .max(2)
        .min(jobs.len());
    let mut outcomes: Vec<_> = std::thread::scope(|threads| {
        let handles: Vec<_> = (1..workers)
            .filter_map(|_| std::thread::Builder::new().spawn_scoped(threads, work).ok())
            .collect();
        let mut outcomes = work();
        for handle in handles {
            outcomes.extend(
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
        outcomes
    });
    outcomes.sort_by_key(|(index, _)| *index);
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

/// Turn evidence into present providers.
///
/// A provider is present in a scope when a signal stronger than a `PATH`
/// probe was found there, or when policy names it and it is on `PATH`.
/// Within an ecosystem the order is the policy's choice first, then by the
/// strongest evidence, then `probe_priority`, the same rule for every
/// ecosystem. Unless policy is strict, a task source no present package
/// manager can run gets the first one on `PATH`, in `probe_priority` order.
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
    let mut present = observed_presence(evidence, policy, registry);
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
            p.because
                .first()
                .map_or((Weight::Probed, 0), Evidence::strength),
            provider.for_present(p).caps.probe_priority,
            provider.id,
        )
    });
    let disagreements = disagreements(&present, registry);
    let mut project = Project {
        present,
        warnings,
        disagreements,
        ..Project::default()
    };
    project.refresh_bins(tree, registry);
    for index in 0..project.present.len() {
        let search = search_dirs(&project.present, &project.present[index].scope);
        let provider = registry.by_id(project.present[index].provider);
        observe_version(
            tree,
            provider,
            &mut project.present[index],
            &search,
            &mut project.warnings,
        );
    }
    add_task_runners(tree, policy, registry, &mut project);
    Ok(project)
}

/// Package managers of one ecosystem and scope where a manifest declares one and a lockfile pins another.
fn disagreements(present: &[Present], registry: &Registry) -> Vec<Disagreement> {
    let strongest = |p: &Present| p.because.first().map(|e| (e.weight, e.at.clone()));
    let managers = present.iter().filter(|p| {
        registry
            .by_id(p.provider)
            .kind
            .contains(Kind::PACKAGE_MANAGER)
    });
    let mut found = Vec::new();
    for declared in managers.clone() {
        let Some((Weight::Declared, manifest)) = strongest(declared) else {
            continue;
        };
        let ecosystem = registry.by_id(declared.provider).ecosystem;
        for locked in managers.clone() {
            if locked.provider == declared.provider
                || locked.scope != declared.scope
                || registry.by_id(locked.provider).ecosystem != ecosystem
            {
                continue;
            }
            let Some((Weight::Locked, lockfile)) = strongest(locked) else {
                continue;
            };
            found.push(Disagreement {
                scope: declared.scope.clone(),
                declared: declared.provider,
                manifest: manifest.clone(),
                locked: locked.provider,
                lockfile,
            });
        }
    }
    found
}

/// Present providers grouped from evidence, before ordering.
fn observed_presence(
    evidence: Vec<Evidence>,
    policy: &Policy,
    registry: &Registry,
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
        because.sort_by_key(Evidence::strength);
        let Some(strongest) = because.first().map(|item| item.weight) else {
            continue;
        };
        if strongest == Weight::Probed && chosen_by(policy, id).is_none() {
            continue;
        }
        present.push(Present {
            provider: id,
            scope,
            version: None,
            bin_dirs: Vec::new(),
            because,
        });
    }
    present
}

/// The executable directories every provider present in `scope` or the root
/// declares.
fn search_dirs(present: &[Present], scope: &Scope) -> Vec<std::path::PathBuf> {
    present
        .iter()
        .filter(|present| present.scope == *scope || present.scope == Scope::Root)
        .flat_map(|present| present.bin_dirs.iter().cloned())
        .collect()
}

/// Ask the installed executable in `search` or on `PATH` its version when
/// the provider parses one, and let that version name the variant when
/// nothing in the project did.
fn observe_version(
    tree: &Tree,
    provider: &Provider,
    observed: &mut Present,
    search: &[std::path::PathBuf],
    warnings: &mut Vec<Warning>,
) {
    let wants_variant = provider.caps.variant_of_version.is_some()
        && !observed
            .because
            .iter()
            .any(|item| matches!(item.declared, Some(crate::Declared::Variant(_))));
    if let Some(version) = provider.version
        && (provider.caps.variant_of_version.is_none() || wants_variant)
    {
        let queried = Present {
            bin_dirs: search.to_vec(),
            ..observed.clone()
        };
        match version(&crate::plan::scope_dir(tree, &observed.scope), &queried) {
            Ok(value) => observed.version = Some(value),
            Err(warning) => warnings.push(warning),
        }
    }
    if wants_variant
        && let Some(derive) = provider.caps.variant_of_version
        && let Some(name) = observed.version.as_deref().and_then(derive)
    {
        let program = provider.program.unwrap_or(provider.label);
        observed.because.push(Evidence {
            provider: Some(observed.provider),
            signal: provider
                .signals
                .iter()
                .position(|signal| matches!(signal, crate::Signal::Probe(name) if *name == program))
                .map(crate::SignalId),
            at: crate::probe::probe_with(program, search).unwrap_or_else(|| program.into()),
            scope: observed.scope.clone(),
            weight: Weight::Probed,
            declared: Some(crate::Declared::Variant(name.into())),
        });
    }
}

/// Lower every lockfile the repository does not track to `Configured` when
/// a tracked lockfile of another provider in the same ecosystem sits beside
/// it in the same scope.
///
/// `tracked` answers `None` when the question cannot be put to the
/// repository, in which case nothing changes.
pub fn prefer_tracked_lockfiles(
    evidence: &mut [Evidence],
    registry: &Registry,
    tracked: &dyn Fn(&std::path::Path) -> Option<bool>,
) {
    let mut groups: BTreeMap<(Scope, Ecosystem), Vec<usize>> = BTreeMap::new();
    for (index, item) in evidence.iter().enumerate() {
        let Some(provider) = item.provider else {
            continue;
        };
        let provider = registry.by_id(provider);
        let is_lockfile = matches!(
            item.signal.and_then(|id| provider.signals.get(id.0)),
            Some(crate::Signal::Lockfile(_))
        );
        if is_lockfile && item.weight == Weight::Locked {
            groups
                .entry((item.scope.clone(), provider.ecosystem))
                .or_default()
                .push(index);
        }
    }
    for indices in groups.values() {
        let mut providers: Vec<ProviderId> = indices
            .iter()
            .filter_map(|index| evidence[*index].provider)
            .collect();
        providers.sort_unstable();
        providers.dedup();
        if providers.len() < 2 {
            continue;
        }
        let mut committed = Vec::new();
        let mut unanswered = false;
        for index in indices {
            match tracked(&evidence[*index].at) {
                Some(true) => committed.push(evidence[*index].provider),
                Some(false) => {}
                None => unanswered = true,
            }
        }
        committed.sort_unstable();
        committed.dedup();
        if unanswered || committed.len() != 1 {
            continue;
        }
        for index in indices {
            if evidence[*index].provider != committed[0] {
                evidence[*index].weight = Weight::Configured;
            }
        }
    }
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
        let supports = |provider: &Provider| {
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
        let bins = search_dirs(&project.present, &scope);
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
        let mut search = synthesised.bin_dirs.clone();
        search.extend(bins);
        observe_version(
            tree,
            provider,
            &mut synthesised,
            &search,
            &mut project.warnings,
        );
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
    fn task_sources_are_read_concurrently_and_merged_in_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        static ARRIVED: AtomicUsize = AtomicUsize::new(0);
        fn meet(present: &crate::Present, _: &Tree) -> Result<crate::Extracted, crate::Warning> {
            ARRIVED.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while ARRIVED.load(Ordering::SeqCst) < 2 {
                if Instant::now() > deadline {
                    return Err(crate::Warning::about(present.provider, "read alone"));
                }
                std::thread::yield_now();
            }
            Ok(vec![crate::Task {
                name: format!("{:?}", present.provider),
                source: present.provider,
                scope: Scope::Root,
                target: None,
                description: None,
                alias_of: None,
                forwards_to: None,
                detail: crate::TaskDetail::default(),
            }]
            .into())
        }
        static SOURCES: &[Provider] = &[
            Provider {
                tasks: Some(meet),
                ..fake(ProviderId::Npm, "npm", Ecosystem::Node)
            },
            Provider {
                tasks: Some(meet),
                ..fake(ProviderId::Uv, "uv", Ecosystem::Python)
            },
        ];
        let project = resolve(
            &tree(),
            vec![
                found(ProviderId::Uv, Weight::Locked),
                found(ProviderId::Npm, Weight::Locked),
            ],
            &Policy::default(),
            &Registry(SOURCES),
        )
        .unwrap();
        assert_eq!(project.unread, []);
        let order: Vec<ProviderId> = project.present.iter().map(|p| p.provider).collect();
        let sources: Vec<ProviderId> = project.tasks.iter().map(|task| task.source).collect();
        assert_eq!(sources, order);
    }

    #[test]
    fn task_extraction_runs_on_a_bounded_number_of_threads() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static RUNNING: AtomicUsize = AtomicUsize::new(0);
        fn bound() -> usize {
            std::thread::available_parallelism()
                .map_or(2, std::num::NonZero::get)
                .max(2)
        }
        fn count(present: &crate::Present, _: &Tree) -> Result<crate::Extracted, crate::Warning> {
            let running = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            if running > bound() {
                return Err(crate::Warning::about(
                    present.provider,
                    format!("{running} extractors at once"),
                ));
            }
            Ok(vec![crate::Task {
                name: format!("{:?}", present.scope),
                source: present.provider,
                scope: present.scope.clone(),
                target: None,
                description: None,
                alias_of: None,
                forwards_to: None,
                detail: crate::TaskDetail::default(),
            }]
            .into())
        }
        static SOURCES: &[Provider] = &[Provider {
            tasks: Some(count),
            ..fake(ProviderId::Npm, "npm", Ecosystem::Node)
        }];
        let members: Vec<Scope> = (0..200)
            .map(|index| Scope::Member {
                name: format!("m{index:03}"),
                dir: PathBuf::from(format!("/p/m{index:03}")),
            })
            .collect();
        let evidence = members
            .iter()
            .map(|scope| Evidence {
                scope: scope.clone(),
                ..found(ProviderId::Npm, Weight::Locked)
            })
            .collect();
        let project = resolve(&tree(), evidence, &Policy::default(), &Registry(SOURCES)).unwrap();
        assert_eq!(project.unread, []);
        let scopes: Vec<&Scope> = project.tasks.iter().map(|task| &task.scope).collect();
        let present: Vec<&Scope> = project.present.iter().map(|p| &p.scope).collect();
        assert_eq!(scopes, present);
        assert_eq!(scopes.len(), members.len());
    }

    #[test]
    fn the_installed_version_names_the_variant_when_nothing_in_the_project_does() {
        fn version(
            _: &std::path::Path,
            present: &crate::Present,
        ) -> Result<String, crate::Warning> {
            present
                .because
                .first()
                .map(|_| "4.1.0".to_owned())
                .ok_or_else(|| crate::Warning::about(present.provider, "no evidence"))
        }
        fn line(version: &str) -> Option<&'static str> {
            version.starts_with('4').then_some("berry")
        }
        const BERRY: Capabilities = Capabilities {
            probe_priority: 9,
            ..Capabilities::NONE
        };
        static VERSIONED: &[Provider] = &[Provider {
            caps: Capabilities {
                variants: &[("berry", BERRY)],
                variant_of_version: Some(line),
                ..Capabilities::NONE
            },
            version: Some(version),
            ..fake(ProviderId::Yarn, "yarn", Ecosystem::Node)
        }];
        let registry = Registry(VERSIONED);
        let mut policy = Policy::default();
        policy.pm.0.insert(
            Ecosystem::Node,
            Choice {
                id: ProviderId::Yarn,
                from: Layer::Cli,
            },
        );
        let project = resolve(
            &tree(),
            vec![found(ProviderId::Yarn, Weight::Probed)],
            &policy,
            &registry,
        )
        .unwrap();
        let yarn = &project.present[0];
        assert!(yarn.because.iter().any(|e| matches!(
            &e.declared,
            Some(crate::Declared::Variant(name)) if name == "berry"
        )));

        assert_eq!(
            registry
                .by_id(ProviderId::Yarn)
                .for_present(yarn)
                .caps
                .probe_priority,
            9
        );

        let mut declared = found(ProviderId::Yarn, Weight::Declared);
        declared.declared = Some(crate::Declared::Variant("berry".into()));
        let project = resolve(&tree(), vec![declared], &policy, &registry).unwrap();
        assert_eq!(project.present[0].version, None);
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
        assert_eq!(project.warnings, []);
        let absent = resolve(&tree(), Vec::new(), &policy, &registry).unwrap();
        assert_eq!(absent.warnings.len(), 1);
    }

    #[test]
    fn an_observed_package_manager_is_versioned_through_the_project_bin_dirs() {
        fn version(
            _: &std::path::Path,
            present: &crate::Present,
        ) -> Result<String, crate::Warning> {
            present
                .bin_dirs
                .iter()
                .any(|dir| dir.join("yarn").is_file())
                .then(|| "4.1.0".to_owned())
                .ok_or_else(|| crate::Warning::about(present.provider, "not in the bin dirs"))
        }
        static VERSIONED: &[Provider] = &[Provider {
            version: Some(version),
            ..fake(ProviderId::Yarn, "yarn", Ecosystem::Node)
        }];
        let dir = crate::probe::tests::TempDir::new("resolve-observed-version");
        let bin = dir.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("yarn"), "").unwrap();
        let tree = Tree {
            cwd: dir.path().to_path_buf(),
            root: dir.path().to_path_buf(),
            members: Vec::new(),
        };
        let project = resolve(
            &tree,
            vec![found(ProviderId::Yarn, Weight::Locked)],
            &Policy::default(),
            &Registry(VERSIONED),
        )
        .unwrap();
        assert_eq!(project.warnings, []);
        assert_eq!(project.present[0].version.as_deref(), Some("4.1.0"));
    }

    #[cfg(unix)]
    #[test]
    fn a_fallback_package_manager_gets_its_installed_version_and_variant() {
        use std::os::unix::fs::PermissionsExt as _;

        fn version(
            dir: &std::path::Path,
            present: &crate::Present,
        ) -> Result<String, crate::Warning> {
            dir.is_dir()
                .then(|| "4.1.0".to_owned())
                .ok_or_else(|| crate::Warning::about(present.provider, "no project directory"))
        }
        fn line(version: &str) -> Option<&'static str> {
            version.starts_with('4').then_some("berry")
        }
        const BERRY: Capabilities = Capabilities {
            probe_priority: 9,
            ..Capabilities::NONE
        };
        static FALLBACK: &[Provider] = &[
            Provider {
                kind: Kind::TASK_SOURCE,
                program: None,
                ..fake(ProviderId::PackageJson, "package.json", Ecosystem::Node)
            },
            Provider {
                signals: &[Signal::Probe("yarn")],
                caps: Capabilities {
                    variants: &[("berry", BERRY)],
                    variant_of_version: Some(line),
                    run_task: Some(crate::capability::RunTaskCap {
                        argv: crate::t![Task, Args],
                        sources: &[ProviderId::PackageJson],
                    }),
                    ..Capabilities::NONE
                },
                version: Some(version),
                ..fake(ProviderId::Yarn, "yarn", Ecosystem::Node)
            },
        ];
        let dir = crate::probe::tests::TempDir::new("resolve-fallback-variant");
        let bin = dir.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        let yarn = bin.join("yarn");
        std::fs::write(&yarn, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&yarn, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tree = Tree {
            cwd: dir.path().to_path_buf(),
            root: dir.path().to_path_buf(),
            members: Vec::new(),
        };
        let manifest = Evidence {
            at: dir.path().join("package.json"),
            ..found(ProviderId::PackageJson, Weight::Present)
        };
        let project = resolve(
            &tree,
            vec![manifest],
            &Policy::default(),
            &Registry(FALLBACK),
        )
        .unwrap();
        let yarn = project
            .present
            .iter()
            .find(|present| present.provider == ProviderId::Yarn)
            .expect("the bin dir supplies yarn");
        assert_eq!(yarn.version.as_deref(), Some("4.1.0"));
        assert!(yarn.because.iter().any(|e| matches!(
            &e.declared,
            Some(crate::Declared::Variant(name)) if name == "berry"
        )));
    }
}
