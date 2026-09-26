//! Turborepo.

use runner_core::{
    Capabilities, CleanCap, Ecosystem, Hooks, Kind, Provider, ProviderId, QuietSupport, RunTaskCap,
    Signal, t,
};

/// Turborepo.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Turbo,
    label: "turbo",
    aliases: &["turbo.json", "turbo.jsonc"],
    ecosystem: Ecosystem::Node,
    kind: Kind::TASK_SOURCE,
    program: Some("turbo"),
    signals: &[
        Signal::File("turbo.json"),
        Signal::File("turbo.jsonc"),
        Signal::Probe("turbo"),
    ],
    caps: Capabilities {
        task_table: runner_core::TaskTable::Key("tasks"),
        task_priority: 0,
        run_task: Some(RunTaskCap {
            argv: t!["run", Task, Sep("--"), Args],
            sources: &[ProviderId::Turbo],
        }),
        clean: Some(CleanCap {
            dir_suffixes: &[],
            framework_dirs: &[],
            dirs: &[".turbo"],
        }),
        quiet: QuietSupport::unsupported("--output-logs suppresses task logs"),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::turbo::tasks),
    version: None,
    hooks: Hooks::NONE,
};
