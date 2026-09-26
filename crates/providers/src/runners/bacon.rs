//! bacon.

use runner_core::{
    Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, QuietSupport, RunTaskCap, Signal, t,
};

/// bacon.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Bacon,
    label: "bacon",
    aliases: &["bacon.toml"],
    ecosystem: Ecosystem::Rust,
    kind: Kind::TASK_SOURCE,
    program: Some("bacon"),
    signals: &[Signal::File("bacon.toml"), Signal::Probe("bacon")],
    caps: Capabilities {
        task_table: runner_core::TaskTable::Key("jobs"),
        task_priority: 8,
        run_default: Some(t![Quiet, Sep("--"), Args]),
        run_task: Some(RunTaskCap {
            argv: t![Task, Sep("--"), Args],
            sources: &[ProviderId::Bacon],
        }),
        quiet: QuietSupport::NONE,
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::bacon::tasks),
    version: None,
    hooks: Hooks::NONE,
};
