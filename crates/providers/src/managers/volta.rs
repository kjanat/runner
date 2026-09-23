//! Volta.

use runner_core::{Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, Signal};

/// Volta.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Volta,
    label: "volta",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::TOOL_MANAGER,
    program: Some("volta"),
    signals: &[Signal::EnvVar("VOLTA_HOME"), Signal::Probe("volta")],
    writes: &[],
    caps: Capabilities::NONE,
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
