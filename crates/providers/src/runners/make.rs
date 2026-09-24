//! GNU Make.

use runner_core::{
    Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, QuietSupport, RunTaskCap, Signal, t,
};

/// GNU Make.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Make,
    label: "make",
    aliases: &["Makefile"],
    ecosystem: Ecosystem::Any,
    kind: Kind::TASK_SOURCE,
    program: Some("make"),
    signals: &[
        Signal::File("Makefile"),
        Signal::File("GNUmakefile"),
        Signal::File("makefile"),
        Signal::Probe("make"),
    ],
    writes: &[],
    caps: Capabilities {
        run_default: Some(t![Quiet, Args]),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, Task, Args],
            sources: &[ProviderId::Make],
        }),
        quiet: QuietSupport::flag(t!["-s"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
