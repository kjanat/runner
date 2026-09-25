//! go-task.

use runner_core::{
    Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, QuietSupport, RunTaskCap, Signal, t,
};

/// go-task.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Task,
    label: "task",
    aliases: &["go-task", "Taskfile"],
    ecosystem: Ecosystem::Any,
    kind: Kind::TASK_SOURCE,
    program: Some("task"),
    signals: &[
        Signal::File("Taskfile.yml"),
        Signal::File("taskfile.yml"),
        Signal::File("Taskfile.yaml"),
        Signal::File("taskfile.yaml"),
        Signal::File("Taskfile.dist.yml"),
        Signal::File("taskfile.dist.yml"),
        Signal::File("Taskfile.dist.yaml"),
        Signal::File("taskfile.dist.yaml"),
        Signal::Probe("task"),
    ],
    writes: &[],
    caps: Capabilities {
        run_default: Some(t![Quiet, Args]),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, Task, Args],
            sources: &[ProviderId::Task],
        }),
        quiet: QuietSupport::flag(t!["-s"]),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::go_task::tasks),
    version: None,
    hooks: Hooks::NONE,
};
