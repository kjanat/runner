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

/// What to do with a plan that can fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReachPolicy {
    /// Prompt on a terminal.
    #[default]
    Ask,
    /// Proceed.
    Allow,
    /// Refuse.
    Local,
}

impl ReachPolicy {
    /// Every value, in the order the declaration table lists them.
    pub const ALL: [Self; 3] = [Self::Ask, Self::Allow, Self::Local];

    /// The `RUNNER_REACH` spelling.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::Local => "local",
        }
    }

    /// The value `label` spells.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|value| value.label() == label.trim())
    }
}

/// What to do when a manifest declares one package manager and a lockfile
/// beside it pins another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OnMismatch {
    /// Take the manifest's word.
    #[default]
    Proceed,
    /// Refuse until a policy layer settles it.
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
    /// The task runner.
    pub runner: Option<Choice>,
    /// Preferred task sources, in order; other sources remain candidates.
    pub prefer: Vec<ProviderId>,
    /// Per-task source rankings from configuration.
    pub task_sources: BTreeMap<String, Vec<ProviderId>>,
    /// The runtime.
    pub runtime: Option<Choice>,
    /// Whether installs must not touch the lockfile.
    pub frozen: bool,
    /// What to do with lifecycle scripts.
    pub scripts: ScriptPolicy,
    /// What to do with a plan that can fetch.
    pub reach: ReachPolicy,
    /// The host diagnostic level.
    pub verbosity: Verbosity,
    /// Divert host diagnostics to stderr where the provider supports it.
    pub host_stderr: bool,
    /// Environment layers.
    pub env: EnvLayers,
    /// The operations each tool manager runs on install.
    pub tool_ops: BTreeMap<ProviderId, Vec<String>>,
    /// Which trust repository config acts at.
    pub trust: TrustPolicy,
    /// Refuse instead of taking a package manager from `PATH` for a task source.
    pub strict: bool,
    /// What to do when a manifest and a lockfile disagree.
    pub on_mismatch: OnMismatch,
}
