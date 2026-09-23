//! Nx.

use runner_core::{
    Capabilities, CleanCap, Ecosystem, Hooks, Kind, Provider, ProviderId, QuietSupport, Signal,
};

/// Nx.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Nx,
    label: "nx",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::TASK_SOURCE,
    program: Some("nx"),
    signals: &[Signal::File("nx.json"), Signal::Probe("nx")],
    writes: &[],
    caps: Capabilities {
        clean: Some(CleanCap { dirs: &[".nx"] }),
        quiet: QuietSupport::NONE,
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
