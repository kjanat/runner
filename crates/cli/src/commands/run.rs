//! `runner run <target>`, resolve a task name to the right tool and execute
//! it. When no task matches, fall back to executing the target as an
//! arbitrary command through the detected package manager (formerly `runner
//! exec`).
//!
//! # Module layout
//!
//! - [`qualify`], reversed-qualifier detection, selection errors, and the
//!   side-effect-free [`qualify::precheck_task`] used by chain mode to bail
//!   before any sibling task runs.
//! - [`dispatch`], turning a task token into a fully-configured
//!   [`std::process::Command`]: warning emission, the resolver chain,
//!   bun-test special case, PM-exec fallback, and per-source `run_cmd`
//!   selection.
//! - [`runtime`], the `--runtime` axis: the per-runtime script / file / exec
//!   builders, which task sources can honour a runtime, and the warnings for
//!   the ones that cannot. Every other module in `commands::run` reads the
//!   override through here.
//!
//! This file owns only the public entry points ([`run`] for inherited
//! stdio, [`dispatch_task_piped`] for the parallel chain executor) and
//! the test module.

use anyhow::Result;

pub(crate) mod core;
pub(crate) mod decision;
mod dispatch;
mod local_dep;
mod qualify;
mod runtime;

pub(crate) use dispatch::refusal_error;
pub(crate) use qualify::{precheck_task, root_runner};

pub(crate) use runtime::{
    honors as runtime_honors, lifecycle_scripts as runtime_lifecycle_scripts,
};

use crate::resolver::ResolutionOverrides;
use crate::types::{ProjectContext, Task};

/// The identity `[tasks.<key>]` config is matched against: `source:name`
/// for root tasks, the full `member:source#name` FQN for member tasks.
pub(crate) fn task_output_key(task: &Task) -> String {
    if task.member.is_some() {
        crate::schema::labels::fqn(task)
    } else {
        format!("{}:{}", task.source.label(), task.name)
    }
}

/// Resolve `task` and run it with inherited stdio, returning the exit
/// code. `test` shorthand: when `task == "test"` and no `test` task exists,
/// runs the ecosystem's built-in test runner. PM-exec fallback for
/// unqualified misses runs the target through `npx`/`bun x`/`pnpm exec`/
/// `deno x`/`uvx`, plus `go run` for Go module/path-shaped targets;
/// otherwise spawns the binary directly from `PATH`.
pub(crate) fn run(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    sink: super::WarningSink<'_>,
) -> Result<i32> {
    run_with_key(ctx, overrides, task, args, sink).map(|(code, _)| code)
}

pub(crate) fn run_with_key(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    mut sink: super::WarningSink<'_>,
) -> Result<(i32, String)> {
    let mut spawn =
        match dispatch::resolve_dispatch(ctx, overrides, task, args, sink.as_deref_mut(), true)? {
            dispatch::Dispatch::Builtin(name) => {
                return crate::run_builtin(
                    ctx,
                    overrides,
                    &name,
                    args,
                    &mut crate::render::out::Out::stdio(),
                    sink,
                )
                .map(|code| (code, name));
            }
            dispatch::Dispatch::Spawn(spawn) => spawn,
        };
    if overrides.explain {
        crate::render::explain::print_command(overrides, spawn.command_mut());
        return Ok((0, spawn.task_key.clone()));
    }
    // Wrap the child's output in a collapsible GitHub Actions group
    // (`runner: <task>`) when enabled. Opened after resolution so the `→`
    // dispatch arrow stays visible above the fold and a resolver error
    // never leaves an empty group; the guard closes the group on drop.
    let key = spawn.task_key.clone();
    let _group = super::task_group(overrides, task, &key);
    Ok((super::exit_code(spawn.status()?), key))
}

/// Spawn a task with piped stdout/stderr and `Stdio::null()` stdin, or name
/// the builtin the token reaches. Used by the parallel chain executor
/// (`chain::exec::run_parallel`), which runs builtins itself.
pub(crate) fn dispatch_task_piped(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    sink: super::WarningSink<'_>,
) -> Result<PipedDispatch> {
    use std::process::Stdio;

    match dispatch::resolve_dispatch(ctx, overrides, task, args, sink, false)? {
        dispatch::Dispatch::Spawn(mut spawn) => {
            spawn
                .command_mut()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            spawn
                .spawn()
                .map(|child| PipedDispatch::Child(child, spawn.task_key.clone()))
        }
        dispatch::Dispatch::Builtin(name) => Ok(PipedDispatch::Builtin(name)),
    }
}

pub(crate) enum PipedDispatch {
    Child(std::process::Child, String),
    Builtin(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::precheck_task;
    use super::qualify::detect_reversed_qualifier;
    use crate::resolver::{
        OverrideOrigin, PmOverride, ResolutionOverrides, RunnerOverride, RuntimeOverride,
    };
    use crate::types::{JsRuntime, PackageManager, ProjectContext, Task, TaskRunner, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        ProjectContext {
            cwd: root.clone(),
            root,
            tasks,
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        }
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        }
    }

    /// The source `runner run <name>` selects among `sources` under `overrides`.
    fn winner(sources: &[TaskSource], name: &str, overrides: &ResolutionOverrides) -> TaskSource {
        let mut ctx = context(sources.iter().map(|source| task(name, *source)).collect());
        crate::tool::test_support::seed_context_with(&mut ctx, overrides);
        let tree = super::core::tree(&ctx);
        let policy = super::core::policy(overrides);
        let project = super::core::project(&ctx).expect("observed");
        super::core::selected_in(&ctx, &tree, &project, &policy, name)
            .expect("selection")
            .expect("a task")
            .source
    }

    fn pm(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..ResolutionOverrides::default()
        }
    }

    fn runtime(runtime: JsRuntime) -> ResolutionOverrides {
        ResolutionOverrides {
            runtime: Some(RuntimeOverride {
                runtime,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        }
    }

    fn pinned(name: &str, sources: Vec<TaskSource>) -> ResolutionOverrides {
        ResolutionOverrides {
            task_source_overrides: BTreeMap::from([(name.to_string(), sources)]),
            ..ResolutionOverrides::default()
        }
    }

    #[test]
    fn detect_reversed_qualifier_catches_task_colon_source() {
        assert_eq!(
            detect_reversed_qualifier("lint:cargo"),
            Some((TaskSource::CargoAliases, "lint"))
        );
        assert_eq!(
            detect_reversed_qualifier("foo:bar:cargo"),
            Some((TaskSource::CargoAliases, "foo:bar"))
        );
    }

    #[test]
    fn detect_reversed_qualifier_ignores_other_shapes() {
        assert!(detect_reversed_qualifier("cargo:lint").is_none());
        assert!(detect_reversed_qualifier("lint").is_none());
        assert!(detect_reversed_qualifier("lint:zoot").is_none());
        assert!(detect_reversed_qualifier("lint:cargo:extra").is_none());
    }

    #[test]
    fn precheck_reversed_qualifier_beats_runner_constraint() {
        let mut ctx = context(vec![]);
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);

        let err = precheck_task(&ctx, &overrides, "lint:cargo")
            .expect_err("reversed qualifier should fail precheck");

        assert!(format!("{err:#}").contains("cargo:lint"));
    }

    #[test]
    fn a_real_task_shaped_like_a_reversed_qualifier_still_runs() {
        let mut ctx = context(vec![task("lint:cargo", TaskSource::Justfile)]);
        crate::tool::test_support::seed_context(&mut ctx);

        precheck_task(&ctx, &ResolutionOverrides::default(), "lint:cargo")
            .expect("the task named lint:cargo is selected");
    }

    #[test]
    fn the_default_order_ranks_turbo_then_package_json_then_the_rest() {
        let none = ResolutionOverrides::default();
        let order = [
            TaskSource::TurboJson,
            TaskSource::PackageJson,
            TaskSource::Makefile,
            TaskSource::Justfile,
            TaskSource::Taskfile,
            TaskSource::DenoJson,
            TaskSource::CargoAliases,
            TaskSource::BaconToml,
            TaskSource::MiseToml,
        ];
        for pair in order.windows(2) {
            assert_eq!(winner(pair, "build", &none), pair[0], "{pair:?}");
            let reversed = [pair[1], pair[0]];
            assert_eq!(winner(&reversed, "build", &none), pair[0], "{pair:?}");
        }
    }

    #[test]
    fn a_forced_package_manager_pulls_its_own_sources_forward() {
        let json_and_deno = [TaskSource::PackageJson, TaskSource::DenoJson];
        for origin in [OverrideOrigin::CliFlag, OverrideOrigin::EnvVar] {
            assert_eq!(
                winner(&json_and_deno, "check", &pm(PackageManager::Deno, origin)),
                TaskSource::DenoJson
            );
        }
        assert_eq!(
            winner(
                &[TaskSource::TurboJson, TaskSource::DenoJson],
                "check",
                &pm(PackageManager::Deno, OverrideOrigin::EnvVar)
            ),
            TaskSource::DenoJson
        );
        assert_eq!(
            winner(
                &[TaskSource::TurboJson, TaskSource::PackageJson],
                "check",
                &pm(PackageManager::Bun, OverrideOrigin::CliFlag)
            ),
            TaskSource::PackageJson
        );
        assert_eq!(
            winner(
                &json_and_deno,
                "check",
                &pm(PackageManager::Composer, OverrideOrigin::CliFlag)
            ),
            TaskSource::PackageJson,
            "a package manager dispatching neither source reorders nothing"
        );
    }

    #[test]
    fn a_forced_runtime_pulls_the_sources_it_dispatches_forward() {
        let turbo_and_json = [TaskSource::TurboJson, TaskSource::PackageJson];
        assert_eq!(
            winner(&turbo_and_json, "build", &runtime(JsRuntime::Bun)),
            TaskSource::PackageJson
        );
        let both = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Cargo,
                origin: OverrideOrigin::CliFlag,
            }),
            ..runtime(JsRuntime::Bun)
        };
        assert_eq!(
            winner(&turbo_and_json, "build", &both),
            TaskSource::PackageJson
        );
    }

    #[test]
    fn the_prefer_list_ranks_and_never_restricts() {
        let turbo_and_json = [TaskSource::TurboJson, TaskSource::PackageJson];
        let prefer = |sources: Vec<TaskSource>| ResolutionOverrides {
            prefer_sources: sources,
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            winner(
                &turbo_and_json,
                "build",
                &prefer(vec![TaskSource::PackageJson, TaskSource::TurboJson])
            ),
            TaskSource::PackageJson
        );
        assert_eq!(
            winner(
                &turbo_and_json,
                "build",
                &prefer(vec![TaskSource::TurboJson, TaskSource::PackageJson])
            ),
            TaskSource::TurboJson
        );
        assert_eq!(
            winner(
                &[TaskSource::Makefile],
                "build",
                &prefer(vec![TaskSource::TurboJson])
            ),
            TaskSource::Makefile
        );
        let runners = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            winner(
                &[TaskSource::TurboJson, TaskSource::Justfile],
                "build",
                &runners
            ),
            TaskSource::Justfile
        );
    }

    #[test]
    fn a_chosen_runner_selects_its_own_task() {
        let overrides = ResolutionOverrides {
            runner: Some(RunnerOverride {
                runner: TaskRunner::Just,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            winner(
                &[TaskSource::TurboJson, TaskSource::Justfile],
                "build",
                &overrides
            ),
            TaskSource::Justfile
        );
    }

    #[test]
    fn a_per_task_pin_decides_its_own_name_below_a_forced_package_manager() {
        let turbo_and_json = [TaskSource::TurboJson, TaskSource::PackageJson];
        assert_eq!(
            winner(
                &turbo_and_json,
                "build",
                &pinned("build", vec![TaskSource::PackageJson])
            ),
            TaskSource::PackageJson
        );
        assert_eq!(
            winner(
                &turbo_and_json,
                "build",
                &pinned("dev", vec![TaskSource::PackageJson])
            ),
            TaskSource::TurboJson
        );
        let forced = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..pinned("build", vec![TaskSource::TurboJson])
        };
        assert_eq!(
            winner(&turbo_and_json, "build", &forced),
            TaskSource::PackageJson
        );
    }
}
