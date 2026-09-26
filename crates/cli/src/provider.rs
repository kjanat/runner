//! Registry lookups for provider labels, kinds and ecosystems.

use runner_core::{Ecosystem, Kind, Provider, ProviderId};
use runner_providers::REGISTRY;

/// A provider's registry entry and the facts read from it.
pub(crate) trait Named: Copy {
    /// The registry entry.
    fn provider(self) -> &'static Provider;

    /// The canonical label.
    fn label(self) -> &'static str {
        self.provider().label
    }

    /// The ecosystem the provider belongs to.
    fn ecosystem(self) -> Ecosystem {
        self.provider().ecosystem
    }

    /// The provider itself when it reads tasks from its own file.
    fn as_task_source(self) -> Option<ProviderId> {
        let provider = self.provider();
        provider.tasks.is_some().then_some(provider.id)
    }

    /// Whether package managers dispatch this task source, since no program
    /// of its own runs it.
    fn is_managed(self) -> bool {
        let provider = self.provider();
        provider.kind.contains(Kind::TASK_SOURCE) && provider.program.is_none()
    }

    /// The task sources the provider dispatches, most native first.
    fn dispatches(self) -> &'static [ProviderId] {
        self.provider().caps.run_task.map_or(&[], |cap| cap.sources)
    }
}

impl Named for ProviderId {
    fn provider(self) -> &'static Provider {
        REGISTRY.by_id(self)
    }
}

const fn is_runner(provider: &Provider) -> bool {
    provider.kind.contains(Kind::TASK_SOURCE)
        && !provider.kind.intersects(Kind::PACKAGE_MANAGER)
        && provider.program.is_some()
}

const fn is_js_runtime(provider: &Provider) -> bool {
    provider.kind.contains(Kind::RUNTIME)
        && matches!(provider.ecosystem, Ecosystem::Node | Ecosystem::Deno)
}

fn parse(spelling: &str, admits: fn(&Provider) -> bool) -> Option<ProviderId> {
    REGISTRY
        .by_label(spelling)
        .filter(|provider| admits(provider))
        .map(|provider| provider.id)
}

fn every(admits: fn(&Provider) -> bool) -> Vec<ProviderId> {
    REGISTRY
        .iter()
        .filter(|provider| admits(provider))
        .map(|provider| provider.id)
        .collect()
}

const fn is_package_manager(provider: &Provider) -> bool {
    provider.kind.contains(Kind::PACKAGE_MANAGER)
}

fn is_task_source(provider: &Provider) -> bool {
    provider.kind.contains(Kind::TASK_SOURCE) && provider.tasks.is_some()
}

/// The package manager `spelling` names.
pub(crate) fn package_manager(spelling: &str) -> Option<ProviderId> {
    parse(spelling, is_package_manager)
}

/// The task runner `spelling` names.
pub(crate) fn runner(spelling: &str) -> Option<ProviderId> {
    parse(spelling, is_runner)
}

/// The task source `spelling` names.
pub(crate) fn task_source(spelling: &str) -> Option<ProviderId> {
    parse(spelling, is_task_source)
}

/// Every task source package managers dispatch, in task priority order.
pub(crate) fn managed_sources() -> Vec<ProviderId> {
    task_sources()
        .into_iter()
        .filter(|source| source.is_managed())
        .collect()
}

/// The package managers that dispatch `source`, in the order the core probes `PATH` for one.
pub(crate) fn dispatchers(source: ProviderId) -> Vec<ProviderId> {
    let mut dispatchers: Vec<ProviderId> = package_managers()
        .into_iter()
        .filter(|pm| pm.dispatches().contains(&source))
        .collect();
    dispatchers.sort_by_key(|pm| pm.provider().caps.probe_priority);
    dispatchers
}

/// Why no package manager dispatches `source`, and how to get one.
pub(crate) fn no_dispatcher(source: ProviderId) -> String {
    let dispatchers: Vec<&str> = dispatchers(source).into_iter().map(Named::label).collect();
    format!(
        "no {} package manager detected to run {} tasks; pin one with `--pm <name>`, set \
         `RUNNER_PM=<name>`, add it to runner.toml, or install one of {}",
        source.ecosystem().label(),
        source.label(),
        dispatchers.join(", "),
    )
}

/// The JavaScript runtime `spelling` names.
pub(crate) fn js_runtime(spelling: &str) -> Option<ProviderId> {
    parse(spelling, is_js_runtime)
}

/// Every package manager, in registry order.
pub(crate) fn package_managers() -> Vec<ProviderId> {
    every(is_package_manager)
}

/// Every task runner, in registry order.
pub(crate) fn runners() -> Vec<ProviderId> {
    every(is_runner)
}

/// Every task source, in task priority order.
pub(crate) fn task_sources() -> Vec<ProviderId> {
    let mut sources = every(is_task_source);
    sources.sort_by_key(|source| source.provider().caps.task_priority);
    sources
}

/// Every JavaScript runtime, in registry order.
pub(crate) fn js_runtimes() -> Vec<ProviderId> {
    every(is_js_runtime)
}

#[cfg(test)]
mod tests {
    use runner_core::ProviderId;

    #[test]
    fn package_json_dispatchers_are_in_probe_order() {
        assert_eq!(
            super::dispatchers(ProviderId::PackageJson),
            [
                ProviderId::Npm,
                ProviderId::Bun,
                ProviderId::Pnpm,
                ProviderId::Yarn,
                ProviderId::Deno,
            ]
        );
    }
}
