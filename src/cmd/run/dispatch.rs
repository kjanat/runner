//! Resolve a task token to a fully-configured [`Command`] (including
//! the `→` arrow trace) and the supporting fallback paths.
//!
//! Three flavors of dispatch share this code:
//! - normal entry: `resolve_dispatch` matched a [`crate::types::Task`]
//!   and builds the per-source run command via [`build_run_command`];
//! - bun-test special case: `runner test` with no `package.json` script
//!   forwards to `bun test` directly;
//! - PM-exec fallback: no task matched, so the token is run through
//!   `npx`/`bunx`/`pnpm exec`/`deno x`/`uvx` or spawned from `$PATH`
//!   directly when the resolver landed on a PM without an exec primitive.

use std::process::Command;

use anyhow::{Result, bail};
use colored::Colorize;

use super::qualify::{
    allowed_runner_sources, detect_reversed_qualifier, parse_qualified_task,
    runner_constraint_error,
};
use super::select::select_task_entry;
use crate::resolver::{ResolutionOverrides, ResolveError, Resolver};
use crate::tool;
use crate::types::{Ecosystem, PackageManager, ProjectContext, Task, TaskSource};

/// Resolve `task` to a fully-configured [`Command`] without spawning it.
///
/// Walks the same cascade for every caller — warning emission, qualified
/// vs unqualified lookup, runner constraint check, resolver chain,
/// bun-test special case, PM-exec fallback, or a normal task entry —
/// and returns a [`Command`] whose working directory + env have already
/// been set via [`crate::cmd::configure_command`]. Callers attach stdio +
/// `.status()` / `.spawn()` according to their needs.
///
/// Fallbacks (resolver + bun-test + PM-exec) are scoped to unqualified
/// lookups so a qualified miss like `runner run justfile:test` bails on
/// the qualifier rather than silently dispatching `bun test`.
///
/// The resolver call lives inside the unqualified branch so qualified
/// misses don't pay for PM resolution (warning emission, potential
/// `<pm> --version` spawn for devEngines.version checks) on an error
/// path they can't reach. Only the soft `NoSignalsFound { soft: true,
/// .. }` outcome collapses to `None` so the direct PATH spawn can still
/// fire for `runner run somebin`. Hard errors — `--fallback=error`,
/// manifest `onFail = Error`, and any other resolver failure —
/// propagate so the user sees the real diagnostic instead of a silent
/// degrade.
pub(super) fn resolve_dispatch(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    mut sink: crate::cmd::WarningSink<'_>,
) -> Result<Command> {
    crate::cmd::print_warnings(ctx, overrides, sink.as_deref_mut());

    let (qualifier, task_name) = parse_qualified_task(task);

    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task_name).collect();

    // `--runner X` / `[task_runner].prefer` is restrictive: when set, a
    // candidate that isn't under one of the allowed sources is treated
    // as non-existent. A qualifier (`runner.json:task`) is the user
    // narrowing *to* a source explicitly and outranks the runner
    // constraint — the qualified branch below applies its own match.
    let restricted: Vec<_> = if qualifier.is_some() {
        found.clone()
    } else if let Some(allowed) = allowed_runner_sources(overrides) {
        found
            .iter()
            .copied()
            .filter(|t| allowed.contains(&t.source))
            .collect()
    } else {
        found.clone()
    };

    if restricted.is_empty() {
        // Restrictive override active but no candidate matched: hard
        // error per the resolved design decision (explicit intent
        // never silently downgrades). Skipped for qualified misses —
        // the qualifier (`justfile:foo`) is stronger user intent than
        // `--runner` / `[task_runner].prefer`, so report the qualified
        // miss directly instead of surfacing a runner-constraint error
        // the user can't act on.
        if qualifier.is_none() {
            // Fast-fail on the reversed qualifier shape (`task:source`).
            // Without this guard, `lint:cargo` slips through as an
            // unqualified bare name, hits the PM-exec fallback below,
            // and surfaces a cryptic `ENOENT` from the OS spawning a
            // binary literally named `lint:cargo`.
            if let Some((src, task_part)) = detect_reversed_qualifier(task) {
                let src_label = src.label();
                bail!(
                    "unknown qualifier in {task:?}: source {src_label:?} must come first.\n\
                     hint: did you mean \"{src_label}:{task_part}\"?",
                );
            }

            if let Some(reason) = runner_constraint_error(overrides, &found) {
                return Err(reason.into());
            }

            let resolved_pm = match Resolver::new(ctx, overrides).resolve_node_pm() {
                Ok(decision) => {
                    crate::cmd::print_warning_slice(
                        &decision.warnings,
                        overrides,
                        sink.as_deref_mut(),
                    );
                    if overrides.explain {
                        eprintln!(
                            "{} {} resolved: {}",
                            "·".dimmed(),
                            "runner".dimmed(),
                            decision.describe(),
                        );
                    }
                    Some(decision.pm)
                }
                Err(ResolveError::NoSignalsFound { soft: true, .. }) => None,
                Err(e) => return Err(e.into()),
            };

            // Bun-test special case: `bun test` built-in.
            if should_use_bun_test_fallback(ctx, resolved_pm, task_name) {
                eprintln!(
                    "{} {} {} {}",
                    "→".dimmed(),
                    "bun".dimmed(),
                    "test".bold(),
                    args.join(" ").dimmed(),
                );
                let mut cmd = tool::bun::test_cmd(args);
                crate::cmd::configure_command(&mut cmd, &ctx.root);
                return Ok(cmd);
            }

            // PM-exec fallback: dispatch through detected PM's exec primitive.
            let (label, mut cmd) = build_pm_exec_command(ctx, resolved_pm, task_name, args);
            eprintln!(
                "{} {} {} {}",
                "→".dimmed(),
                label.dimmed(),
                task_name.bold(),
                args.join(" ").dimmed(),
            );
            crate::cmd::configure_command(&mut cmd, &ctx.root);
            return Ok(cmd);
        }

        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    let entry = if let Some(source) = qualifier {
        restricted
            .iter()
            .find(|t| t.source == source)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("task {task_name:?} not found in {}", source.label()))?
    } else {
        select_task_entry(ctx, overrides, &restricted)
    };

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        entry.source.label().dimmed(),
        task_name.bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = build_run_command(ctx, overrides, entry, args, sink)?;
    crate::cmd::configure_command(&mut cmd, &ctx.root);
    Ok(cmd)
}

/// Build the command for the PM-exec fallback path. Used by both
/// `super::run` (inherit stdio) and `super::dispatch_task_piped`
/// (piped stdio).
fn build_pm_exec_command(
    ctx: &ProjectContext,
    resolved_pm: Option<PackageManager>,
    task_name: &str,
    args: &[String],
) -> (&'static str, Command) {
    let combined = || {
        let mut v = Vec::with_capacity(args.len() + 1);
        v.push(task_name.to_string());
        v.extend(args.iter().cloned());
        v
    };
    let direct_exec = || {
        let mut c = tool::program::command(task_name);
        c.args(args);
        ("exec", c)
    };
    match resolved_pm {
        Some(PackageManager::Npm) => ("npm", tool::npm::exec_cmd(&combined())),
        Some(PackageManager::Yarn) => ("yarn", tool::yarn::exec_cmd(&ctx.root, &combined())),
        Some(PackageManager::Pnpm) => ("pnpm", tool::pnpm::exec_cmd(&combined())),
        Some(PackageManager::Bun) => ("bun", tool::bun::exec_cmd(&combined())),
        Some(PackageManager::Deno) => ("deno x", tool::deno::exec_cmd(&combined())),
        Some(PackageManager::Uv) => ("uvx", tool::uv::exec_cmd(&combined())),
        Some(PackageManager::Go) => {
            if task_name.contains('@') || task_name.contains('/') || task_name.contains('\\') {
                ("go run", tool::go_pm::exec_cmd(&combined()))
            } else {
                direct_exec()
            }
        }
        None | Some(_) => direct_exec(),
    }
}

/// Bun special-case for `runner test` when the project has no
/// `package.json` `test` script: forward to `bun test`.
///
/// `resolved_pm` is the verdict from the full resolver chain, so all
/// signals — `--pm`, `RUNNER_PM`, `runner.toml`, `packageManager`,
/// `devEngines.packageManager`, lockfile, PATH probe — get a vote.
/// Fires only when the resolver landed on Bun.
pub(super) fn should_use_bun_test_fallback(
    ctx: &ProjectContext,
    resolved_pm: Option<PackageManager>,
    task: &str,
) -> bool {
    if task != "test" || has_package_script(ctx, task) {
        return false;
    }
    resolved_pm.is_some_and(|pm| pm == PackageManager::Bun)
}

fn has_package_script(ctx: &ProjectContext, task: &str) -> bool {
    ctx.tasks
        .iter()
        .any(|entry| entry.source == TaskSource::PackageJson && entry.name == task)
}

/// Build a [`Command`] for the given task source and package manager.
fn build_run_command(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    entry: &Task,
    args: &[String],
    sink: crate::cmd::WarningSink<'_>,
) -> Result<Command> {
    Ok(match entry.source {
        TaskSource::TurboJson => tool::turbo::run_cmd(&entry.name, args),
        TaskSource::PackageJson => {
            let decision = Resolver::new(ctx, overrides).resolve_node_pm()?;
            crate::cmd::print_warning_slice(&decision.warnings, overrides, sink);
            if overrides.explain {
                eprintln!(
                    "{} {} resolved: {}",
                    "·".dimmed(),
                    "runner".dimmed(),
                    decision.describe(),
                );
            }
            let pm = decision.pm;
            match pm {
                PackageManager::Npm => tool::npm::run_cmd(&entry.name, args),
                PackageManager::Yarn => tool::yarn::run_cmd(&entry.name, args),
                PackageManager::Pnpm => tool::pnpm::run_cmd(&entry.name, args),
                PackageManager::Bun => tool::bun::run_cmd(&entry.name, args),
                PackageManager::Deno => tool::deno::run_cmd(&entry.name, args),
                other => bail!("{} cannot run scripts", other.label()),
            }
        }
        TaskSource::Makefile => tool::make::run_cmd(&entry.name, args),
        TaskSource::Justfile => tool::just::run_cmd(&entry.name, args),
        TaskSource::Taskfile => tool::go_task::run_cmd(&entry.name, args),
        TaskSource::DenoJson => tool::deno::run_cmd(&entry.name, args),
        TaskSource::CargoAliases => tool::cargo_aliases::run_cmd(&entry.name, args),
        TaskSource::GoPackage => {
            let Some(run_target) = entry.run_target.as_deref() else {
                bail!("go task {:?} is missing its run target", entry.name);
            };
            tool::go_pm::run_cmd(run_target, args)
        }
        TaskSource::BaconToml => tool::bacon::run_cmd(&entry.name, args),
        TaskSource::MiseToml => tool::mise::run_cmd(&entry.name, args),
        TaskSource::PyprojectScripts => {
            let Some(pm) = resolve_python_pm(ctx, overrides) else {
                bail!(
                    "no Python package manager detected to run {:?}; install uv, poetry, or pipenv",
                    entry.name,
                );
            };
            match pm {
                PackageManager::Uv => tool::uv::run_cmd(&entry.name, args),
                PackageManager::Poetry => tool::poetry::run_cmd(&entry.name, args),
                PackageManager::Pipenv => tool::pipenv::run_cmd(&entry.name, args),
                other => bail!("{} cannot run pyproject scripts", other.label()),
            }
        }
    })
}

/// Pick the Python package manager that dispatches a `[project.scripts]`
/// entry: an explicit Python-ecosystem `--pm` / `RUNNER_PM` override
/// first, then a `[pm].python` `runner.toml` override, then the PM
/// detected for the project. A non-Python `--pm` (e.g. `--pm pnpm` in a
/// mixed repo) is ignored here rather than forced, falling through to the
/// detected Python PM.
fn resolve_python_pm(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Option<PackageManager> {
    if let Some(o) = overrides.pm.as_ref()
        && o.pm.ecosystem() == Ecosystem::Python
    {
        return Some(o.pm);
    }
    if let Some(o) = overrides.pm_by_ecosystem.get(&Ecosystem::Python) {
        return Some(o.pm);
    }
    ctx.package_managers
        .iter()
        .copied()
        .find(|pm| pm.ecosystem() == Ecosystem::Python)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{build_pm_exec_command, resolve_dispatch};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{PackageManager, ProjectContext, Task, TaskRunner, TaskSource};

    fn context() -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn command_args(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn resolve_dispatch_reversed_qualifier_beats_runner_constraint() {
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };

        let err = resolve_dispatch(&context(), &overrides, "lint:cargo", &[], None)
            .expect_err("reversed qualifier should fail dispatch");

        assert!(format!("{err:#}").contains("cargo:lint"));
    }

    #[test]
    fn resolve_dispatch_go_package_uses_recorded_task_source() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Go);
        ctx.tasks.push(Task {
            name: "serve".to_string(),
            source: TaskSource::GoPackage,
            run_target: Some("./cmd/serve".to_string()),
            description: None,
            alias_of: None,
            passthrough_to: None,
        });
        let args = [String::from("--port"), String::from("3000")];

        let command = resolve_dispatch(&ctx, &ResolutionOverrides::default(), "serve", &args, None)
            .expect("go package task should dispatch");

        assert_eq!(command.get_program().to_string_lossy(), "go");
        assert_eq!(
            command_args(&command),
            ["run", "./cmd/serve", "--port", "3000"]
        );
    }

    #[test]
    fn resolve_dispatch_pyproject_script_uses_uv_run() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Uv);
        ctx.tasks.push(Task {
            name: "greenpy".to_string(),
            source: TaskSource::PyprojectScripts,
            run_target: None,
            description: Some("greenpy.main:main".to_string()),
            alias_of: None,
            passthrough_to: None,
        });
        let args = [String::from("--flag")];

        let command = resolve_dispatch(
            &ctx,
            &ResolutionOverrides::default(),
            "greenpy",
            &args,
            None,
        )
        .expect("pyproject script should dispatch");

        assert_eq!(command.get_program().to_string_lossy(), "uv");
        assert_eq!(command_args(&command), ["run", "greenpy", "--flag"]);
    }

    #[test]
    fn resolve_dispatch_pyproject_script_uses_poetry_run_when_detected() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Poetry);
        ctx.tasks.push(Task {
            name: "greenpy".to_string(),
            source: TaskSource::PyprojectScripts,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        });

        let command = resolve_dispatch(&ctx, &ResolutionOverrides::default(), "greenpy", &[], None)
            .expect("pyproject script should dispatch");

        assert_eq!(command.get_program().to_string_lossy(), "poetry");
        assert_eq!(command_args(&command), ["run", "greenpy"]);
    }

    #[test]
    fn build_pm_exec_command_go_versioned_uses_go_run() {
        let args = [String::from("--help")];
        let (label, command) = build_pm_exec_command(
            &context(),
            Some(PackageManager::Go),
            "github.com/foo/tool@v1.2.3",
            &args,
        );

        assert_eq!(label, "go run");
        assert_eq!(command.get_program().to_string_lossy(), "go");
        assert_eq!(
            command_args(&command),
            ["run", "github.com/foo/tool@v1.2.3", "--help"],
        );
    }

    #[test]
    fn build_pm_exec_command_go_import_path_uses_go_run() {
        let (label, command) = build_pm_exec_command(
            &context(),
            Some(PackageManager::Go),
            "github.com/foo/tool",
            &[],
        );

        assert_eq!(label, "go run");
        assert_eq!(command.get_program().to_string_lossy(), "go");
        assert_eq!(command_args(&command), ["run", "github.com/foo/tool"]);
    }

    #[test]
    fn build_pm_exec_command_go_relative_path_uses_go_run() {
        let (label, command) =
            build_pm_exec_command(&context(), Some(PackageManager::Go), "./cmd/foo", &[]);

        assert_eq!(label, "go run");
        assert_eq!(command.get_program().to_string_lossy(), "go");
        assert_eq!(command_args(&command), ["run", "./cmd/foo"]);
    }

    #[test]
    fn build_pm_exec_command_go_windows_path_uses_go_run() {
        let (label, command) =
            build_pm_exec_command(&context(), Some(PackageManager::Go), ".\\cmd\\foo", &[]);

        assert_eq!(label, "go run");
        assert_eq!(command.get_program().to_string_lossy(), "go");
        assert_eq!(command_args(&command), ["run", ".\\cmd\\foo"]);
    }

    #[test]
    fn build_pm_exec_command_go_bare_name_falls_through_to_path() {
        let args = [String::from("run")];
        let (label, command) =
            build_pm_exec_command(&context(), Some(PackageManager::Go), "golangci-lint", &args);

        assert_eq!(label, "exec");
        assert_eq!(command.get_program().to_string_lossy(), "golangci-lint");
        assert_eq!(command_args(&command), ["run"]);
    }
}
