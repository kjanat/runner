//! The override chain, resolved once per invocation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::env::EnvLayers;
use crate::provider::{Ecosystem, ProviderId};
use crate::verbosity::Verbosity;

/// Where a setting came from, strongest first.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    /// A command-line flag.
    Cli,
    /// A `RUNNER_*` variable.
    Env,
    /// A `runner.toml`.
    ConfigFile(PathBuf),
    /// A manifest field.
    Manifest(PathBuf),
    /// A lockfile.
    Lockfile(PathBuf),
    /// The executable on `PATH`.
    Probe,
}

/// A provider chosen by one layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The provider.
    pub id: ProviderId,
    /// The layer that chose it.
    pub from: Layer,
}

/// One value per ecosystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerEcosystem<T>(pub BTreeMap<Ecosystem, T>);

impl<T> Default for PerEcosystem<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<T> PerEcosystem<T> {
    /// The value for `ecosystem`.
    #[must_use]
    pub fn get(&self, ecosystem: Ecosystem) -> Option<&T> {
        self.0.get(&ecosystem)
    }
}

/// What to do with lifecycle scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScriptPolicy {
    /// The provider's own default.
    #[default]
    Default,
    /// Skip them.
    Deny,
    /// Run them.
    Allow,
}

/// Whether a plan that downloads may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Download {
    /// Run it after the user confirms.
    #[default]
    Ask,
    /// Run it.
    Allow,
    /// Refuse it.
    Refuse,
}

/// Which trust level a repository config may act at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TrustPolicy {
    /// Repository config is project trust.
    #[default]
    Project,
    /// Repository config is user trust.
    Full,
}

/// The override chain, resolved once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// The package manager per ecosystem.
    pub pm: PerEcosystem<Choice>,
    /// The task source that must supply the task.
    pub source: Option<Choice>,
    /// The runtime.
    pub runtime: Option<Choice>,
    /// Providers a task's own settings choose, present when only `PATH`
    /// shows them.
    pub named: Vec<ProviderId>,
    /// Whether installs must not touch the lockfile.
    pub frozen: bool,
    /// What to do with lifecycle scripts.
    pub scripts: ScriptPolicy,
    /// Whether a plan that downloads may run.
    pub download: Download,
    /// The host diagnostic level.
    pub verbosity: Verbosity,
    /// Environment layers.
    pub env: EnvLayers,
    /// Which trust repository config acts at.
    pub trust: TrustPolicy,
}
