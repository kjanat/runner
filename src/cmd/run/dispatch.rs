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
use crate::types::{PackageManager, ProjectContext, TaskSource};

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
        if qualifier.is_none()
            && let Some(reason) = runner_constraint_error(overrides, &found)
        {
            return Err(reason.into());
        }

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

    let mut cmd = build_run_command(ctx, overrides, entry.source, task_name, args, sink)?;
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
    match resolved_pm {
        Some(PackageManager::Npm) => ("npm", tool::npm::exec_cmd(&combined())),
        Some(PackageManager::Yarn) => ("yarn", tool::yarn::exec_cmd(&ctx.root, &combined())),
        Some(PackageManager::Pnpm) => ("pnpm", tool::pnpm::exec_cmd(&combined())),
        Some(PackageManager::Bun) => ("bun", tool::bun::exec_cmd(&combined())),
        Some(PackageManager::Deno) => ("deno x", tool::deno::exec_cmd(&combined())),
        Some(PackageManager::Uv) => ("uvx", tool::uv::exec_cmd(&combined())),
        // Go intentionally falls through to direct PATH spawn alongside
        // Cargo/Poetry/Pipenv/Bundler/Composer. `go run <name>` only
        // works for Go module paths (`example.com/foo@v1`, `./main.go`, `.`),
        // not arbitrary tools the user wants to exec — so it
        // isn't a comparable PM-exec primitive like `npx`/`bunx`/`uvx`.
        None | Some(_) => {
            let mut c = tool::program::command(task_name);
            c.args(args);
            ("exec", c)
        }
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
    source: TaskSource,
    task: &str,
    args: &[String],
    sink: crate::cmd::WarningSink<'_>,
) -> Result<Command> {
    Ok(match source {
        TaskSource::TurboJson => tool::turbo::run_cmd(task, args),
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
                PackageManager::Npm => tool::npm::run_cmd(task, args),
                PackageManager::Yarn => tool::yarn::run_cmd(task, args),
                PackageManager::Pnpm => tool::pnpm::run_cmd(task, args),
                PackageManager::Bun => tool::bun::run_cmd(task, args),
                PackageManager::Deno => tool::deno::run_cmd(task, args),
                other => bail!("{} cannot run scripts", other.label()),
            }
        }
        TaskSource::Makefile => tool::make::run_cmd(task, args),
        TaskSource::Justfile => tool::just::run_cmd(task, args),
        TaskSource::Taskfile => tool::go_task::run_cmd(task, args),
        TaskSource::DenoJson => tool::deno::run_cmd(task, args),
        TaskSource::CargoAliases => tool::cargo_aliases::run_cmd(task, args),
        TaskSource::BaconToml => tool::bacon::run_cmd(task, args),
        TaskSource::MiseToml => tool::mise::run_cmd(task, args),
    })
}
