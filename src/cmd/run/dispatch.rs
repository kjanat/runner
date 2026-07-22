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
    TokenLookup, allowed_runner_sources, detect_reversed_qualifier, lookup_token,
    qualified_miss_error, reversed_qualifier_error, runner_constraint_error,
};
use super::select::select_task_entry;
use crate::resolver::{OverrideOrigin, ResolutionOverrides, ResolveError, Resolver};
use crate::tool;
use crate::tool::deno_exec::DenoTaskPlan;
use crate::types::{Ecosystem, JsRuntime, PackageManager, ProjectContext, Task, TaskSource};

fn print_dispatch_arrow(
    overrides: &ResolutionOverrides,
    label: &str,
    task_name: &str,
    args: &[String],
) {
    if overrides.silences_runner() {
        return;
    }
    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        label.dimmed(),
        task_name.bold(),
        args.join(" ").dimmed(),
    );
}

fn print_pm_explain(overrides: &ResolutionOverrides, describe: &str) {
    if !overrides.explain || overrides.silences_runner() {
        return;
    }
    eprintln!(
        "{} {} resolved: {}",
        "·".dimmed(),
        "runner".dimmed(),
        describe,
    );
}

/// Outcome of resolving a task: a spawnable process, or a deno task to
/// run in-process via the embedded task shell.
#[derive(Debug)]
pub(super) enum Dispatch {
    /// A configured process to spawn (`.status()` / `.spawn()`).
    Spawn(Command),
    /// A deno task resolved for in-process execution (no `deno` binary).
    DenoSelfExec(DenoSelfExec),
}

/// A deno task resolved for in-process execution.
#[derive(Debug)]
pub(super) struct DenoSelfExec {
    plan: DenoTaskPlan,
    args: Vec<String>,
    cwd: std::path::PathBuf,
}

impl DenoSelfExec {
    /// Run the task in-process, returning its exit code.
    pub(super) fn run(&self) -> Result<i32> {
        tool::deno_exec::run(&self.plan, &self.args, &self.cwd)
    }
}

/// Whether a `deno` binary is resolvable on `$PATH`.
fn deno_present() -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var_os("PATHEXT");
    crate::resolver::probe_path_for_doctor("deno", &path, pathext.as_deref()).is_some()
}

/// Decide whether to run a deno `entry` in-process instead of spawning
/// `deno task`. Returns `Ok(Some(_))` to self-exec, `Ok(None)` to fall
/// through to `deno task`, or `Err` when deno is required (the task has
/// dependencies or invokes `deno`) but isn't installed.
///
/// Default policy self-execs only as a fallback when deno is absent; the
/// `unstable-deno-exec` feature makes self-exec primary.
fn decide_deno_self_exec(
    ctx: &ProjectContext,
    entry: &Task,
    args: &[String],
    allow_self_exec: bool,
) -> Result<Option<DenoSelfExec>> {
    if entry.source != TaskSource::DenoJson {
        return Ok(None);
    }
    let deno = deno_present();
    let self_exec_first = cfg!(feature = "unstable-deno-exec");
    if !allow_self_exec || (deno && !self_exec_first) {
        return Ok(None);
    }

    let plan = tool::deno::find_config_upwards(&ctx.root)
        .and_then(|path| tool::deno_exec::plan(&path, &entry.name));
    match plan {
        Some(plan) if plan.self_executable() => Ok(Some(DenoSelfExec {
            plan,
            args: args.to_vec(),
            cwd: ctx.root.clone(),
        })),
        // Not self-executable: real deno can still run it; otherwise bail.
        _ if deno => Ok(None),
        _ => bail!(
            "task {:?} needs deno (it has dependencies or invokes `deno`), but deno is not \
             installed",
            entry.name
        ),
    }
}

/// Resolve `task` to a fully-configured [`Command`] without spawning it.
///
/// Walks the same cascade for every caller, warning emission, qualified
/// vs unqualified lookup, runner constraint check, resolver chain,
/// bun-test special case, PM-exec fallback, or a normal task entry,
/// and returns a [`Command`] whose working directory + env have already
/// been set via [`crate::cmd::configure_command`]. Callers attach stdio +
/// `.status()` / `.spawn()` according to their needs.
///
/// Fallbacks (resolver + bun-test + PM-exec) are scoped to unqualified
/// lookups so a qualified miss like `runner run justfile:test` bails on
/// the qualifier rather than silently dispatching `bun test`.
///
/// The resolver call sits inside the unqualified branch so qualified
/// misses skip PM resolution entirely. Only a soft `NoSignalsFound`
/// collapses to `None` (letting `runner run somebin` direct-spawn);
/// hard errors (`--fallback=error`, manifest `onFail = Error`, …)
/// propagate so the user sees the real diagnostic.
pub(super) fn resolve_dispatch(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    mut sink: crate::cmd::WarningSink<'_>,
    allow_self_exec: bool,
) -> Result<Dispatch> {
    crate::cmd::print_warnings(ctx, overrides, sink.as_deref_mut());

    // Local-file execution short-circuit: a token with an explicit
    // local-path prefix (`./`, `../`, `/`, `\`, `~`, or a Windows drive
    // root) that resolves to an existing file is run as that file
    // (executable / shebang / source-by-runtime) and must never reach the
    // PM-exec fallback, which would treat a local path as a remote package
    // spec. Runs before task lookup so an explicit path outranks a
    // same-named task. A separator-bearing but *relative* token (`bin/tool`)
    // is intentionally left for the after-miss `try_bare_file` fallback so a
    // matching task (e.g. a `make bin/tool` target) wins first.
    if let Some(local) = super::local_file::try_path_token(ctx, overrides, task, args)? {
        let mut command = local.command;
        print_dispatch_arrow(overrides, &local.label, task, args);
        crate::cmd::configure_command(&mut command, &ctx.root, overrides);
        return Ok(Dispatch::Spawn(command));
    }

    let (lookup, found) = lookup_token(ctx, task);
    let TokenLookup {
        qualifier,
        task_name,
    } = lookup;

    // `--runner X` / `[task_runner].prefer` is restrictive: when set, a
    // candidate that isn't under one of the allowed sources is treated
    // as non-existent. A qualifier (`runner.json:task`) is the user
    // narrowing *to* a source explicitly and outranks the runner
    // constraint; the qualified branch below applies its own match.
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
        // never silently downgrades). Skipped for qualified misses,
        // the qualifier (`justfile:foo`) is stronger user intent than
        // `--runner` / `[task_runner].prefer`, so report the qualified
        // miss directly instead of surfacing a runner-constraint error
        // the user can't act on.
        let Some(missed_source) = qualifier else {
            // Fast-fail on the reversed qualifier shape (`task:source`).
            // Without this guard, `lint:cargo` slips through as an
            // unqualified bare name, hits the PM-exec fallback below,
            // and surfaces a cryptic `ENOENT` from the OS spawning a
            // binary literally named `lint:cargo`.
            if let Some((src, task_part)) = detect_reversed_qualifier(task) {
                return Err(reversed_qualifier_error(ctx, task, src, task_part));
            }

            if let Some(reason) = runner_constraint_error(overrides, &found) {
                return Err(reason.into());
            }

            // Local file without an explicit prefix: a token that names a
            // runnable file under the project root, a bare name (`main.ts`,
            // `build.sh`) or a relative path with a separator (`bin/tool`), runs
            // as that file. Sits *after* the runner-constraint hard error (an
            // explicit `--runner` never silently downgrades to a coincidental
            // file) but *before* PM resolution, so a local file still runs when
            // node-PM resolution would hard-error for reasons unrelated to it (a
            // strict devEngines/`packageManager` mismatch, an incompatible
            // `--pm`); running `main.ts` via its runtime doesn't need the
            // package.json PM. Tasks already matched above (`restricted` is empty
            // here), so this never shadows a same-named task, and `bunx`/`npx`
            // never sees a local file.
            if let Some(local) = super::local_file::try_bare_file(ctx, overrides, task_name, args)?
            {
                let mut command = local.command;
                print_dispatch_arrow(overrides, &local.label, task_name, args);
                crate::cmd::configure_command(&mut command, &ctx.root, overrides);
                return Ok(Dispatch::Spawn(command));
            }

            // Locally installed dependency: run the binary its manifest
            // declares. Sits alongside the local-file branch, before PM
            // resolution, for the same reason, an installed package needs no
            // package manager to run, and `npx` would treat the already-present
            // directory as a registry spec (an npm alias resolves to a 404).
            if let Some(dep) =
                super::local_dep::try_installed_package(ctx, overrides, task_name, args)?
            {
                let mut command = dep.dispatch.command;
                print_pm_explain(overrides, &dep.describe);
                print_dispatch_arrow(overrides, &dep.dispatch.label, task_name, args);
                crate::cmd::configure_command(&mut command, &ctx.root, overrides);
                return Ok(Dispatch::Spawn(command));
            }

            let resolved_pm = match Resolver::new(ctx, overrides).resolve_node_pm() {
                Ok(decision) => {
                    crate::cmd::print_warning_slice(
                        &decision.warnings,
                        overrides,
                        sink.as_deref_mut(),
                    );
                    print_pm_explain(overrides, &decision.describe());
                    Some(decision.pm)
                }
                Err(ResolveError::NoSignalsFound { soft: true, .. }) => None,
                Err(e) => return Err(e.into()),
            };

            // Bun-test special case: `bun test` built-in.
            if should_use_bun_test_fallback(ctx, resolved_pm, task_name) {
                print_dispatch_arrow(overrides, "bun", "test", args);
                let mut cmd = tool::bun::test_cmd(args);
                crate::cmd::configure_command(&mut cmd, &ctx.root, overrides);
                return Ok(Dispatch::Spawn(cmd));
            }

            // PM-exec fallback: dispatch through detected PM's exec primitive.
            let (label, mut cmd) = build_pm_exec_command(ctx, resolved_pm, task_name, args);
            print_dispatch_arrow(overrides, label, task_name, args);
            crate::cmd::configure_command(&mut cmd, &ctx.root, overrides);
            return Ok(Dispatch::Spawn(cmd));
        };

        // Qualified miss (colon or FQN syntax): the qualifier is explicit
        // task-lookup intent, so error here, never fall through to
        // PM-exec, which would hand the token to bunx/npx as a package
        // spec and resolve it off the network.
        return Err(qualified_miss_error(ctx, missed_source, task_name));
    }

    let entry = if let Some(source) = qualifier {
        restricted
            .iter()
            .find(|t| t.source == source)
            .copied()
            .ok_or_else(|| qualified_miss_error(ctx, source, task_name))?
    } else {
        select_task_entry(ctx, overrides, &restricted)
    };

    // Refuse a task already being dispatched further up this process
    // lineage. Checked before anything is spawned or run in-process, and
    // before the arrow, so the diagnostic is the only output a cycle
    // produces.
    let task_stack = crate::cmd::push_task_frame(&ctx.root, entry.source, &entry.name)?;

    // Deno tasks may run in-process via the embedded task shell (no deno
    // binary) per policy; otherwise fall through to `deno task`.
    if let Some(self_exec) = decide_deno_self_exec(ctx, entry, args, allow_self_exec)? {
        print_dispatch_arrow(overrides, "deno-shell", task_name, args);
        return Ok(Dispatch::DenoSelfExec(self_exec));
    }

    print_dispatch_arrow(overrides, entry.source.label(), task_name, args);

    let mut cmd = build_run_command(ctx, overrides, entry, args, sink)?;
    crate::cmd::configure_command(&mut cmd, &ctx.root, overrides);
    cmd.env(crate::cmd::TASK_STACK_ENV, task_stack);
    Ok(Dispatch::Spawn(cmd))
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
        Some(PackageManager::Npm) => ("npx", tool::npm::exec_cmd(&combined())),
        Some(PackageManager::Yarn) => ("yarn exec", tool::yarn::exec_cmd(&ctx.root, &combined())),
        Some(PackageManager::Pnpm) => ("pnpm exec", tool::pnpm::exec_cmd(&combined())),
        Some(PackageManager::Bun) => ("bunx", tool::bun::exec_cmd(&combined())),
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

/// Python package manager decision for `[project.scripts]` dispatch.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPythonPm {
    pub(crate) pm: PackageManager,
    via: PythonPmResolution,
}

#[derive(Debug, Clone)]
enum PythonPmResolution {
    Override(OverrideOrigin),
    DetectedProject,
}

impl ResolvedPythonPm {
    pub(crate) fn describe(&self) -> String {
        match &self.via {
            PythonPmResolution::Override(OverrideOrigin::CliFlag) => {
                format!("{} via --pm (CLI override)", self.pm.label())
            }
            PythonPmResolution::Override(OverrideOrigin::EnvVar) => {
                format!("{} via RUNNER_PM (environment)", self.pm.label())
            }
            PythonPmResolution::Override(OverrideOrigin::ConfigFile { path }) => {
                format!("{} via runner.toml at {}", self.pm.label(), path.display())
            }
            PythonPmResolution::DetectedProject => {
                format!("{} via detected Python project", self.pm.label())
            }
        }
    }
}

/// Bun special-case for `runner test` when the project has no
/// `package.json` `test` script: forward to `bun test`.
///
/// `resolved_pm` is the verdict from the full resolver chain, so all
/// signals, `--pm`, `RUNNER_PM`, `runner.toml`, `packageManager`,
/// `devEngines.packageManager`, lockfile, PATH probe, get a vote.
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
    // Deep-merge the global (CLI/env) quiet level + stream over this task's
    // `[tasks.<name>].verbosity` config into the flags the host tool gets.
    let hv = overrides.host_verbosity_for(&entry.name);
    Ok(match entry.source {
        TaskSource::TurboJson => tool::turbo::run_cmd(&entry.name, args, hv),
        TaskSource::PackageJson => {
            // An explicit runtime override outranks the resolved PM: which
            // runtime the script's process tree runs on is a separate question
            // from who installed the dependencies, and both bun and deno read
            // `package.json` scripts whatever wrote the lockfile.
            if let Some(over) = overrides.runtime.as_ref() {
                print_pm_explain(overrides, &over.describe());
                return runtime_run_command(ctx, overrides, entry, args, hv, sink, over.runtime);
            }
            let decision = Resolver::new(ctx, overrides).resolve_node_pm()?;
            crate::cmd::print_warning_slice(&decision.warnings, overrides, sink);
            print_pm_explain(overrides, &decision.describe());
            let pm = decision.pm;
            match pm {
                PackageManager::Npm => tool::npm::run_cmd(&entry.name, args, hv),
                PackageManager::Yarn => tool::yarn::run_cmd(&entry.name, args, hv),
                PackageManager::Pnpm => tool::pnpm::run_cmd(&entry.name, args, hv),
                PackageManager::Bun => tool::bun::run_cmd(&entry.name, args, hv),
                PackageManager::Deno => tool::deno::run_cmd(&entry.name, args, hv),
                other => bail!("{} cannot run scripts", other.label()),
            }
        }
        TaskSource::Makefile => tool::make::run_cmd(&entry.name, args, hv),
        TaskSource::Justfile => tool::just::run_cmd(&entry.name, args, hv),
        TaskSource::Taskfile => tool::go_task::run_cmd(&entry.name, args, hv),
        TaskSource::DenoJson => tool::deno::run_cmd(&entry.name, args, hv),
        TaskSource::CargoAliases => tool::cargo_aliases::run_cmd(&entry.name, args, hv),
        TaskSource::GoPackage => {
            let Some(run_target) = entry.run_target.as_deref() else {
                bail!("go task {:?} is missing its run target", entry.name);
            };
            tool::go_pm::run_cmd(run_target, args, hv)
        }
        TaskSource::BaconToml => tool::bacon::run_cmd(&entry.name, args, hv),
        TaskSource::MiseToml => tool::mise::run_cmd(&entry.name, args, hv),
        TaskSource::PyprojectScripts => {
            let Some(decision) = resolve_python_pm(ctx, overrides) else {
                bail!(
                    "no Python package manager detected to run {:?}; install uv, poetry, or pipenv",
                    entry.name,
                );
            };
            print_pm_explain(overrides, &decision.describe());
            let pm = decision.pm;
            match pm {
                PackageManager::Uv => tool::uv::run_cmd(&entry.name, args, hv),
                PackageManager::Poetry => tool::poetry::run_cmd(&entry.name, args, hv),
                PackageManager::Pipenv => tool::pipenv::run_cmd(&entry.name, args, hv),
                other => bail!("{} cannot run pyproject scripts", other.label()),
            }
        }
    })
}

/// Build the command for a `package.json` script under an explicit
/// `--runtime`.
///
/// Bun forces its own runtime onto the whole process tree (`bun --bun run`),
/// which is the thing `--pm bun` could never express. Deno runs the script
/// through `deno task`, which reads `package.json` scripts as well as
/// `deno.json` tasks. Node has no forcing primitive of its own, so it means
/// "whatever the PM is, do not force anything else", and falls through to the
/// resolved node PM.
///
/// A runtime whose binary the project has no sign of is still dispatched: the
/// user named it explicitly, and `bun --bun run` works regardless of which PM
/// wrote the lockfile. A missing binary surfaces as the spawn error naming it,
/// which is more actionable than a pre-emptive guess about what is installed.
fn runtime_run_command(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    entry: &Task,
    args: &[String],
    hv: tool::HostVerbosity,
    sink: crate::cmd::WarningSink<'_>,
    runtime: JsRuntime,
) -> Result<Command> {
    Ok(match runtime {
        JsRuntime::Bun => tool::bun::run_cmd_with_runtime(&entry.name, args, hv, true),
        JsRuntime::Deno => tool::deno::run_cmd(&entry.name, args, hv),
        JsRuntime::Node => {
            let decision = Resolver::new(ctx, overrides).resolve_node_pm()?;
            crate::cmd::print_warning_slice(&decision.warnings, overrides, sink);
            match decision.pm {
                PackageManager::Npm => tool::npm::run_cmd(&entry.name, args, hv),
                PackageManager::Yarn => tool::yarn::run_cmd(&entry.name, args, hv),
                PackageManager::Pnpm => tool::pnpm::run_cmd(&entry.name, args, hv),
                // Plain `bun run`, no `--bun`: bun drives the script but a
                // node-shebanged bin inside it still resolves to system Node.
                PackageManager::Bun => tool::bun::run_cmd(&entry.name, args, hv),
                other => bail!(
                    "--runtime node needs a Node package manager to run scripts, but {} was \
                     resolved for this project",
                    other.label(),
                ),
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
pub(crate) fn resolve_python_pm(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Option<ResolvedPythonPm> {
    if let Some(o) = overrides.pm.as_ref()
        && o.pm.ecosystem() == Ecosystem::Python
    {
        return Some(ResolvedPythonPm {
            pm: o.pm,
            via: PythonPmResolution::Override(o.origin.clone()),
        });
    }
    if let Some(o) = overrides.pm_by_ecosystem.get(&Ecosystem::Python) {
        return Some(ResolvedPythonPm {
            pm: o.pm,
            via: PythonPmResolution::Override(o.origin.clone()),
        });
    }
    ctx.package_managers
        .iter()
        .copied()
        .find(|pm| pm.ecosystem() == Ecosystem::Python)
        .map(|pm| ResolvedPythonPm {
            pm,
            via: PythonPmResolution::DetectedProject,
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use std::process::Command;

    use super::{Dispatch, build_pm_exec_command, resolve_dispatch};
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
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn expect_command(dispatch: Dispatch) -> Command {
        match dispatch {
            Dispatch::Spawn(command) => command,
            Dispatch::DenoSelfExec(_) => panic!("expected a spawnable command, got deno self-exec"),
        }
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn justfile_task(name: &str) -> Task {
        Task {
            name: name.to_string(),
            source: TaskSource::Justfile,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn resolve_dispatch_reaches_colon_named_task_shadowed_by_source_label() {
        // ts-x509 bug: `run deno:importsmap` parsed `deno` as a source
        // qualifier and never found the task literally named
        // `deno:importsmap`, falling through to PM-exec instead.
        let mut ctx = context();
        ctx.tasks.push(justfile_task("deno:importsmap"));

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "deno:importsmap",
                &[],
                None,
                true,
            )
            .expect("colon-named task should dispatch"),
        );

        assert_eq!(command.get_program().to_string_lossy(), "just");
        assert!(command_args(&command).contains(&"deno:importsmap".to_string()));
    }

    #[test]
    fn resolve_dispatch_accepts_doctor_fqn_syntax() {
        // `doctor --json` / `why --json` print `root:<source>#<name>` as a
        // task's identity; running that string must dispatch the task.
        let mut ctx = context();
        ctx.tasks.push(justfile_task("fmt"));

        for token in ["root:just#fmt", "just#fmt"] {
            let command = expect_command(
                resolve_dispatch(
                    &ctx,
                    &ResolutionOverrides::default(),
                    token,
                    &[],
                    None,
                    true,
                )
                .unwrap_or_else(|e| panic!("FQN {token} should dispatch: {e:#}")),
            );
            assert_eq!(command.get_program().to_string_lossy(), "just");
            assert!(command_args(&command).contains(&"fmt".to_string()));
        }
    }

    #[test]
    fn resolve_dispatch_accepts_v3_cargo_alias_fqn() {
        // Schema v3 labels cargo alias tasks `cargo-alias`, so doctor/why
        // print `root:cargo-alias#<name>`; that exact string must run.
        let mut ctx = context();
        ctx.tasks.push(Task {
            name: "b".to_string(),
            source: TaskSource::CargoAliases,
            run_target: None,
            description: None,
            alias_of: Some("build".to_string()),
            passthrough_to: None,
        });

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "root:cargo-alias#b",
                &[],
                None,
                true,
            )
            .expect("v3 cargo-alias FQN should dispatch"),
        );

        assert_eq!(command.get_program().to_string_lossy(), "cargo");
    }

    #[test]
    fn resolve_dispatch_fqn_miss_errors_instead_of_pm_exec() {
        // Previously `run root:package.json#nope` fell through to
        // PM-exec and bunx tried to resolve it as a GitHub package spec
        // off the network. A `#` FQN miss must be a hard error.
        let err = resolve_dispatch(
            &context(),
            &ResolutionOverrides::default(),
            "root:package.json#nope",
            &[],
            None,
            true,
        )
        .expect_err("FQN miss must not reach PM-exec");

        assert!(format!("{err:#}").contains("not found in package.json"));
    }

    #[test]
    fn resolve_dispatch_github_spec_still_reaches_pm_exec() {
        // `user/repo#ref` is a legit bunx/npx package spec; its prefix
        // is not a source label, so it must keep flowing to PM-exec.
        let dispatch = resolve_dispatch(
            &context(),
            &ResolutionOverrides::default(),
            "user/repo#ref",
            &[],
            None,
            true,
        )
        .expect("package spec should fall through to PM-exec");

        let command = expect_command(dispatch);
        let token = "user/repo#ref".to_string();
        assert!(
            command.get_program().to_string_lossy() == token
                || command_args(&command).contains(&token),
            "PM-exec should carry the spec verbatim",
        );
    }

    #[test]
    fn resolve_dispatch_reversed_qualifier_beats_runner_constraint() {
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };

        let err = resolve_dispatch(&context(), &overrides, "lint:cargo", &[], None, true)
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

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "serve",
                &args,
                None,
                true,
            )
            .expect("go package task should dispatch"),
        );

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

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "greenpy",
                &args,
                None,
                true,
            )
            .expect("pyproject script should dispatch"),
        );

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

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "greenpy",
                &[],
                None,
                true,
            )
            .expect("pyproject script should dispatch"),
        );

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
