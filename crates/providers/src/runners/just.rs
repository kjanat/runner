//! just.

use runner_core::{
    Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, QuietSupport, RunTaskCap, Signal, t,
};

/// just.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Just,
    label: "just",
    aliases: &["justfile"],
    ecosystem: Ecosystem::Any,
    kind: Kind::TASK_SOURCE,
    program: Some("just"),
    signals: &[
        Signal::FileCaseless("justfile"),
        Signal::FileCaseless(".justfile"),
        Signal::Probe("just"),
    ],
    writes: &[],
    caps: Capabilities {
        task_priority: 3,
        run_default: Some(t![Quiet, Args]),
        run_task: Some(RunTaskCap {
            argv: t![Task, Args],
            sources: &[ProviderId::Just],
        }),
        quiet: QuietSupport::unsupported("--quiet suppresses task output"),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::just::tasks),
    version: None,
    hooks: Hooks::NONE,
};
