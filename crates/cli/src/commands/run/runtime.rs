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

use crate::provider::Named;
use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, Task};
use runner_core::{Ecosystem, ProviderId};

/// The runtime `--runtime`, `RUNNER_RUNTIME` or `[runtime] javascript`
/// selected, if any.
pub(super) fn overridden(overrides: &ResolutionOverrides) -> Option<ProviderId> {
    overrides.js_runtime()
}

/// Whether the runtime axis replaces the exec primitive the resolver picked.
///
/// It replaces a JS one (`npx`, `yarn exec`, `pnpm exec`, `bun x`, `deno x`)
/// and a resolver that found nothing at all. A Python/Go/Rust/Ruby/PHP
/// project's exec primitive is left alone: there is no JS process to move.
pub(super) fn replaces_exec(resolved_pm: Option<ProviderId>) -> bool {
    resolved_pm.is_none_or(|pm| matches!(pm.ecosystem(), Ecosystem::Node | Ecosystem::Deno))
}

/// The task sources the runtime's own `run_task` capability accepts.
pub(super) fn honored_sources(runtime: ProviderId) -> Vec<ProviderId> {
    runner_providers::REGISTRY
        .by_label(runtime.label())
        .and_then(|provider| provider.caps.run_task)
        .map_or_else(Vec::new, |cap| {
            cap.sources
                .iter()
                .filter_map(|id| {
                    crate::provider::task_source(runner_providers::REGISTRY.by_id(*id).label)
                })
                .collect()
        })
}

/// Whether a task from `source` dispatches on `runtime`.
pub(crate) fn honors(source: ProviderId, runtime: ProviderId) -> bool {
    honored_sources(runtime).contains(&source)
}

/// Report a runtime the selected task, addressed by `key`, cannot honour.
///
/// Called once, at the single point where a matched task's source is known
/// and before anything is built for it.
pub(super) fn report_unhonored(
    overrides: &ResolutionOverrides,
    entry: &Task,
    key: &str,
    sink: crate::commands::WarningSink<'_>,
) {
    let (name, source) = (&entry.name, entry.source);
    let Some(runtime) = overrides.runtime_for(key).map(|over| over.runtime) else {
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
    runtime: ProviderId,
    resolved_pm: Option<ProviderId>,
    sink: crate::commands::WarningSink<'_>,
) {
    let source = resolved_pm.map_or("PATH", Named::label);
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

#[cfg(test)]
mod tests {
    use super::{honors, replaces_exec};
    use crate::provider::Named;
    use runner_core::ProviderId;

    fn script(runtime: ProviderId, args: &[String]) -> (String, Vec<String>) {
        let provider = runner_providers::REGISTRY
            .by_label(runtime.label())
            .unwrap();
        let root = std::env::temp_dir();
        let tree = runner_core::Tree {
            cwd: root.clone(),
            root: root.clone(),
            members: vec![],
        };
        let present = runner_core::Present {
            provider: provider.id,
            scope: runner_core::Scope::Root,
            version: None,
            bin_dirs: vec![],
            because: vec![runner_core::Evidence {
                provider: Some(provider.id),
                signal: None,
                at: root,
                scope: runner_core::Scope::Root,
                weight: runner_core::Weight::Declared,
                declared: None,
            }],
        };
        let policy = runner_core::Policy {
            runtime: Some(runner_core::Choice {
                id: provider.id,
                from: runner_core::Layer::Cli,
            }),
            ..runner_core::Policy::default()
        };
        let task = runner_core::Task {
            name: "build".into(),
            source: ProviderId::PackageJson,
            scope: runner_core::Scope::Root,
            target: None,
            description: None,
            alias_of: None,
            forwards_to: None,
            detail: runner_core::TaskDetail::default(),
        };
        let plan = runner_core::plan_with(
            &tree,
            &runner_core::Project::default(),
            &policy,
            &present,
            &runner_core::Op::Run { task: &task, args },
            &runner_providers::REGISTRY,
        )
        .unwrap();
        (
            plan.argv[0].to_string_lossy().into_owned(),
            plan.argv[1..]
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        )
    }

    #[test]
    fn node_scripts_need_an_injected_double_dash() {
        let (program, args) = script(ProviderId::Node, &[String::from("--watch")]);
        assert_eq!(program, "node");
        assert_eq!(args, ["--run", "build", "--", "--watch"]);
    }

    #[test]
    fn bun_scripts_take_args_directly_under_a_forced_runtime() {
        let (program, args) = script(ProviderId::Bun, &[String::from("--watch")]);
        assert_eq!(program, "bun");
        assert_eq!(args, ["--bun", "run", "build", "--watch"]);
    }

    #[test]
    fn deno_scripts_must_not_get_a_double_dash() {
        // `deno task build -- --watch` forwards the `--` into the task's argv.
        let (program, args) = script(ProviderId::Deno, &[String::from("--watch")]);
        assert_eq!(program, "deno");
        assert_eq!(args, ["task", "build", "--watch"]);
    }

    #[test]
    fn no_runtime_emits_a_trailing_bare_double_dash_without_args() {
        assert_eq!(script(ProviderId::Node, &[]).1, ["--run", "build"]);
        assert_eq!(script(ProviderId::Bun, &[]).1, ["--bun", "run", "build"]);
        assert_eq!(script(ProviderId::Deno, &[]).1, ["task", "build"]);
    }

    #[test]
    fn exec_replacement_is_scoped_to_js_ecosystems() {
        assert!(replaces_exec(None));
        assert!(replaces_exec(Some(ProviderId::Pnpm)));
        assert!(replaces_exec(Some(ProviderId::Deno)));
        assert!(!replaces_exec(Some(ProviderId::Uv)));
        assert!(!replaces_exec(Some(ProviderId::Go)));
        assert!(!replaces_exec(Some(ProviderId::Cargo)));
    }

    #[test]
    fn package_json_honors_every_runtime_and_deno_json_only_deno() {
        for runtime in crate::provider::js_runtimes() {
            assert!(honors(ProviderId::PackageJson, runtime));
            assert!(!honors(ProviderId::Turbo, runtime));
            assert!(!honors(ProviderId::Just, runtime));
        }
        assert!(honors(ProviderId::Deno, ProviderId::Deno));
        assert!(!honors(ProviderId::Deno, ProviderId::Bun));
        assert!(!honors(ProviderId::Deno, ProviderId::Node));
    }
}
