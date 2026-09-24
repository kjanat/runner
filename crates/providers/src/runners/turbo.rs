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
    writes: &[],
    caps: Capabilities {
        task_priority: 0,
        run_task: Some(RunTaskCap {
            argv: t!["run", Task, Sep("--"), Args],
            sources: &[ProviderId::Turbo],
        }),
        clean: Some(CleanCap { dirs: &[".turbo"] }),
        quiet: QuietSupport::unsupported("--output-logs suppresses task logs"),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
