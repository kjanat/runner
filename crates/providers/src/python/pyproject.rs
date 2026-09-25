//! The `pyproject.toml` `[project.scripts]` table.

use runner_core::{Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, Signal};

/// `pyproject.toml`.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Pyproject,
    label: "pyproject.toml",
    aliases: &["pyproject"],
    ecosystem: Ecosystem::Python,
    kind: Kind::TASK_SOURCE,
    program: None,
    signals: &[Signal::ManifestField {
        files: &["pyproject.toml"],
        path: "project.scripts",
        parse: super::table,
    }],
    writes: &[],
    caps: Capabilities::NONE,
    tasks: Some(crate::extract::scripts::python_tasks),
    version: None,
    hooks: Hooks::NONE,
};
