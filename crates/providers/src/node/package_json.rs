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
    caps: Capabilities {
        task_table: runner_core::TaskTable::Key("scripts"),
        task_priority: 1,
        packages: Some(runner_core::PackagesCap {
            installed: super::packages::node_modules,
        }),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::scripts::package_tasks),
    version: None,
    hooks: Hooks::NONE,
};
