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
    caps: Capabilities {
        task_table: runner_core::TaskTable::Name,
        task_priority: 2,
        run_default: Some(t![Quiet, Args]),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, Task, Args],
            sources: &[ProviderId::Make],
        }),
        quiet: QuietSupport::flag(t!["-s"]),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::make::tasks),
    version: None,
    hooks: Hooks {
        before_plan: Some(before_plan),
        after_observe: None,
    },
};

/// Refuse a make target, or a script that is a bare `make <name>` wrapper,
/// given anything but variable assignments, since make would take the word
/// as an option or a goal.
fn before_plan(
    _: &runner_core::Tree,
    _: &runner_core::Present,
    op: &runner_core::Op<'_>,
    _: &runner_core::Policy,
    _: &mut Vec<runner_core::Warning>,
) -> Result<(), runner_core::Refusal> {
    let runner_core::Op::Run { task, args } = op else {
        return Ok(());
    };
    let Some(word) = crate::extract::make::first_non_assignment(args) else {
        return Ok(());
    };
    let subject = if task.source == ProviderId::Make {
        format!("make target {:?}", task.name)
    } else {
        format!(
            "{} script {:?}, which runs `make {}`,",
            crate::REGISTRY.by_id(task.source).label,
            task.name,
            task.name
        )
    };
    Err(runner_core::Refusal::Invalid(format!(
        "{subject} cannot take {word:?}: GNU make has no recipe-argument passthrough, so it would \
         parse the word as its own option or as another goal. Pass `NAME=value` assignments the \
         Makefile reads as `$(NAME)`, or invoke the command itself by path (`run \
         ./node_modules/.bin/<command> <args>`), which outranks task lookup"
    )))
}
