//! The `package.json` scripts table.

use runner_core::{Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, Signal};

/// `package.json`.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::PackageJson,
    label: "package.json",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::TASK_SOURCE,
    program: None,
    signals: &[
        Signal::File("package.json"),
        Signal::File("package.json5"),
        Signal::File("package.yaml"),
    ],
    writes: &[],
    caps: Capabilities {
        task_priority: 1,
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
