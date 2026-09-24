//! The `--runtime` axis: which JS runtime executes a task, a local file, or
//! an ad-hoc binary, independent of which package manager wrote the lockfile.
//!
//! Every dispatch path in `commands::run` asks this module rather than reading
//! [`ResolutionOverrides::js_runtime`] itself, so the flag can never be
//! honoured in one branch and dropped in another.
//!
//! Each runtime brings its own script runner, its own file runner, and its own
//! package-exec primitive:
//!
//! | runtime | script                  | file            | exec         |
//! |---------|-------------------------|-----------------|--------------|
//! | node    | `node --run <task> --`  | `node <file>`   | `npx`        |
//! | bun     | `bun --bun run <task>`  | `bun <file>`    | `bun x --bun` |
//! | deno    | `deno task <task>`      | `deno run <file>` | `deno x`   |
//!
//! Argument forwarding differs per runtime and is not interchangeable.
//! `node --run <task> --flag` exits with `node: bad option: --flag`, so node
//! needs an injected `--`. `deno task <task> -- --flag` forwards the `--`
//! literally into the task's argv, so deno must not get one. bun accepts args
//! directly.

#[cfg(test)]
use std::process::Command;

use crate::resolver::ResolutionOverrides;
#[cfg(test)]
use crate::tool;
use crate::types::{DetectionWarning, JsRuntime, PackageManager, ProjectContext, Task, TaskSource};

/// The runtime an explicit `--runtime` / `RUNNER_RUNTIME` / `[runtime].js`
/// selected, if any.
pub(super) fn overridden(overrides: &ResolutionOverrides) -> Option<JsRuntime> {
    overrides.js_runtime()
}

/// Build the `package.json`-script command for `runtime`.
#[cfg(test)]
pub(super) fn script_cmd(
    runtime: JsRuntime,
    task: &str,
    args: &[String],
    verbosity: tool::HostVerbosity,
) -> Command {
    match runtime {
        JsRuntime::Node => tool::node::run_cmd(task, args, verbosity),
        JsRuntime::Bun => tool::bun::run_cmd_with_runtime(task, args, verbosity, true),
        JsRuntime::Deno => tool::deno::run_cmd(task, args, verbosity),
    }
}

/// Whether the runtime axis replaces the exec primitive the resolver picked.
///
/// It replaces a JS one (`npx`, `yarn exec`, `pnpm exec`, `bun x`, `deno x`)
/// and a resolver that found nothing at all. A Python/Go/Rust/Ruby/PHP
/// project's exec primitive is left alone: there is no JS process to move.
pub(super) fn replaces_exec(resolved_pm: Option<PackageManager>) -> bool {
    resolved_pm.is_none_or(|pm| pm.is_node() || pm == PackageManager::Deno)
}

/// The task sources `runtime` can dispatch, most-native first, mirroring
/// [`PackageManager::owned_task_sources`].
///
/// `package.json` scripts are readable by all three runtimes. `deno.json`
/// tasks run through `deno task` and nothing else. Every other source (turbo,
/// make, just, Taskfile, cargo, go, bacon, mise, pyproject) dispatches through
/// a tool that has no JS runtime to select.
pub(super) const fn honored_sources(runtime: JsRuntime) -> &'static [TaskSource] {
    match runtime {
        JsRuntime::Node | JsRuntime::Bun => &[TaskSource::PackageJson],
        JsRuntime::Deno => &[TaskSource::DenoJson, TaskSource::PackageJson],
    }
}

/// Whether a task from `source` dispatches on `runtime`.
pub(crate) fn honors(source: TaskSource, runtime: JsRuntime) -> bool {
    honored_sources(runtime).contains(&source)
}

/// Effective script-command preview matching what [`script_cmd`] dispatches
/// under a forced runtime, for `why` / `doctor`. `Some` only when `runtime`
/// actually dispatches `source`; the runtime then reads the script through its
/// own runner and the resolved package manager is not consulted.
pub(crate) fn script_preview(runtime: JsRuntime, source: TaskSource, task: &str) -> Option<String> {
    if !honors(source, runtime) {
        return None;
    }
    Some(match runtime {
        JsRuntime::Node => format!("node --run {task}"),
        JsRuntime::Bun => format!("bun --bun run {task}"),
        JsRuntime::Deno => format!("deno task {task}"),
    })
}

/// Report a runtime override the selected task cannot honour.
///
/// Called once, at the single point where a matched task's source is known
/// and before anything is built for it.
pub(super) fn report_unhonored(
    overrides: &ResolutionOverrides,
    entry: &Task,
    sink: crate::commands::WarningSink<'_>,
) {
    report_unhonored_source(overrides, &entry.name, entry.source, sink);
}

/// [`report_unhonored`] for a dispatch that has a source but no task entry,
/// such as a task runner's own entry point.
pub(super) fn report_unhonored_source(
    overrides: &ResolutionOverrides,
    name: &str,
    source: TaskSource,
    sink: crate::commands::WarningSink<'_>,
) {
    let Some(runtime) = overridden(overrides) else {
        return;
    };
    if honors(source, runtime) {
        return;
    }
    crate::commands::print_explain(
        overrides,
        &format!(
            "runtime {} not applied: {name} dispatches through {}",
            runtime.label(),
            source.label(),
        ),
    );
    crate::commands::print_warning_slice(
        &[DetectionWarning::RuntimeNotApplied {
            runtime,
            source: source.label(),
        }],
        overrides,
        sink,
    );
}

/// Report a runtime override the exec fallback cannot honour, i.e. the token
/// is about to run through a non-JS ecosystem's exec primitive.
pub(super) fn report_unapplied_exec(
    overrides: &ResolutionOverrides,
    runtime: JsRuntime,
    resolved_pm: Option<PackageManager>,
    sink: crate::commands::WarningSink<'_>,
) {
    let source = resolved_pm.map_or("PATH", PackageManager::label);
    crate::commands::print_explain(
        overrides,
        &format!(
            "runtime {} not applied: exec runs through {source}",
            runtime.label()
        ),
    );
    crate::commands::print_warning_slice(
        &[DetectionWarning::RuntimeNotApplied { runtime, source }],
        overrides,
        sink,
    );
}

/// Warn when `node --run` will skip lifecycle scripts the project defines.
///
/// Node's `--run` deliberately omits `pre<task>` / `post<task>`, unlike `npm
/// run`, `bun run` and `deno task`, which all run them. Silent omission turns
/// a generated-source `prebuild` into a stale build that fails somewhere else,
/// so name the scripts that will not run.
pub(super) fn warn_skipped_lifecycle(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    sink: crate::commands::WarningSink<'_>,
) {
    let skipped = lifecycle_scripts(ctx, task);
    if skipped.is_empty() {
        return;
    }
    crate::commands::print_warning_slice(
        &[DetectionWarning::NodeRunSkipsLifecycle {
            task: task.to_string(),
            skipped,
        }],
        overrides,
        sink,
    );
}

/// The `pre<task>` / `post<task>` scripts `package.json` actually declares.
pub(crate) fn lifecycle_scripts(ctx: &ProjectContext, task: &str) -> Vec<String> {
    [format!("pre{task}"), format!("post{task}")]
        .into_iter()
        .filter(|name| {
            ctx.tasks
                .iter()
                .any(|entry| entry.source == TaskSource::PackageJson && entry.name == *name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{honors, replaces_exec, script_cmd};
    use crate::tool::HostVerbosity;
    use crate::types::{JsRuntime, PackageManager, TaskSource};

    fn argv(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn script(runtime: JsRuntime, args: &[String]) -> (String, Vec<String>) {
        let cmd = script_cmd(runtime, "build", args, HostVerbosity::default());
        (cmd.get_program().to_string_lossy().into_owned(), argv(&cmd))
    }

    #[test]
    fn node_scripts_need_an_injected_double_dash() {
        let (program, args) = script(JsRuntime::Node, &[String::from("--watch")]);
        assert_eq!(program, "node");
        assert_eq!(args, ["--run", "build", "--", "--watch"]);
    }

    #[test]
    fn bun_scripts_take_args_directly_under_a_forced_runtime() {
        let (program, args) = script(JsRuntime::Bun, &[String::from("--watch")]);
        assert_eq!(program, "bun");
        assert_eq!(args, ["--bun", "run", "build", "--watch"]);
    }

    #[test]
    fn deno_scripts_must_not_get_a_double_dash() {
        // `deno task build -- --watch` forwards the `--` into the task's argv.
        let (program, args) = script(JsRuntime::Deno, &[String::from("--watch")]);
        assert_eq!(program, "deno");
        assert_eq!(args, ["task", "build", "--watch"]);
    }

    #[test]
    fn no_runtime_emits_a_trailing_bare_double_dash_without_args() {
        assert_eq!(script(JsRuntime::Node, &[]).1, ["--run", "build"]);
        assert_eq!(script(JsRuntime::Bun, &[]).1, ["--bun", "run", "build"]);
        assert_eq!(script(JsRuntime::Deno, &[]).1, ["task", "build"]);
    }

    #[test]
    fn exec_replacement_is_scoped_to_js_ecosystems() {
        assert!(replaces_exec(None));
        assert!(replaces_exec(Some(PackageManager::Pnpm)));
        assert!(replaces_exec(Some(PackageManager::Deno)));
        assert!(!replaces_exec(Some(PackageManager::Uv)));
        assert!(!replaces_exec(Some(PackageManager::Go)));
        assert!(!replaces_exec(Some(PackageManager::Cargo)));
    }

    #[test]
    fn package_json_honors_every_runtime_and_deno_json_only_deno() {
        for runtime in JsRuntime::all() {
            assert!(honors(TaskSource::PackageJson, *runtime));
            assert!(!honors(TaskSource::TurboJson, *runtime));
            assert!(!honors(TaskSource::Justfile, *runtime));
        }
        assert!(honors(TaskSource::DenoJson, JsRuntime::Deno));
        assert!(!honors(TaskSource::DenoJson, JsRuntime::Bun));
        assert!(!honors(TaskSource::DenoJson, JsRuntime::Node));
    }
}
