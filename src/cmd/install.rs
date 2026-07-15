//! `runner install`, install dependencies via every detected package manager.

use std::any::Any;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};
use crate::resolver::{CollisionPolicy, ResolutionOverrides, ResolveError, Resolver, ScriptPolicy};
use crate::tool;
use crate::types::{DetectionWarning, PackageManager, ProjectContext, TaskRunner, version_matches};

/// Install dependencies for each detected package manager.
///
/// Warns when the current Node.js version doesn't match the project's
/// expected version before proceeding. Thin wrapper over [`install_pms`]
/// that preserves the package manager's actual exit code for callers.
pub(crate) fn install(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    frozen: bool,
) -> Result<i32> {
    install_pms(ctx, overrides, frozen, None)
}

/// Chain-aware install entry. Runs install across every detected PM and
/// returns the first failing PM's exit code, or 0 if all succeed.
///
/// `_sink` is accepted for chain-mode parity with `cmd::run::run`; today
/// install dispatch doesn't emit detection warnings of its own (those
/// flow through the resolver path), so the sink is unused. Kept on the
/// signature so future warning-emitting install paths slot in without a
/// breaking change.
///
/// Used by `chain::exec` when `ChainItemKind::Install` appears as a
/// chain item (i.e. `runner install <tasks>`).
pub(crate) fn install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    frozen: bool,
    _sink: super::WarningSink<'_>,
) -> Result<i32> {
    if ctx.package_managers.is_empty() {
        bail!("No package manager detected.");
    }

    // Planned before the GHA group opens so a refused override doesn't
    // emit an empty `runner: install` group, same rationale as the
    // no-PM bail above.
    let plan = plan_install(ctx, overrides)?;

    report_plan(&plan, overrides);
    warn_unsupported_script_policy(&plan.pms, overrides);

    // Collapse the whole install (single- or multi-PM) under one
    // `runner: install` GitHub Actions group when enabled.
    let _group = super::task_group(overrides, "install");

    if let (Some(nv), Some(cur)) = (&ctx.node_version, &ctx.current_node)
        && !version_matches(&nv.expected, cur)
    {
        eprintln!(
            "{} node expected {} ({}), current {}",
            "warn:".yellow().bold(),
            nv.expected,
            nv.source,
            cur,
        );
        suggest_version_switch(ctx);
    }

    if let [pm] = plan.pms.as_slice() {
        return install_single(ctx, *pm, frozen, overrides);
    }

    run_installs_parallel(ctx, &plan, frozen, overrides)
}

/// Which PMs this invocation installs with, in precedence order:
///
/// 1. The cross-ecosystem `--pm`/`RUNNER_PM` override (which also affects
///    script dispatch), installs with that PM alone; errors if it isn't
///    detected.
/// 2. The install-scoped allowlist `RUNNER_INSTALL_PMS` / `[install].pms`
///    (resolved into `overrides.install_pms`), installs with the detected
///    PMs in that list, preserving detection order; errors if a listed PM
///    isn't detected.
/// 3. Otherwise every detected PM.
///
/// `pm_by_ecosystem` (runner.toml `[pm].node`/`[pm].python`) is
/// deliberately NOT consulted: it scopes *script dispatch* to an
/// ecosystem. The `[install]` allowlist is the install-fan-out knob.
fn select_install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Result<Vec<PackageManager>, ResolveError> {
    if let Some(o) = &overrides.pm {
        if !ctx.package_managers.contains(&o.pm) {
            return Err(ResolveError::PmOverrideNotDetected {
                pm: o.pm,
                origin: o.origin.clone(),
                detected: ctx.package_managers.clone(),
            });
        }
    } else {
        let missing: Vec<PackageManager> = overrides
            .install_pms
            .iter()
            .copied()
            .filter(|pm| !ctx.package_managers.contains(pm))
            .collect();
        if !missing.is_empty() {
            return Err(ResolveError::InstallPmsNotDetected {
                missing,
                detected: ctx.package_managers.clone(),
            });
        }
    }

    Ok(effective_install_pms(ctx, overrides))
}

/// The same selection as [`select_install_pms`] with the not-detected
/// errors dropped: an override naming an absent PM narrows to nothing
/// instead of failing. Reporting surfaces need the effective set without
/// inheriting dispatch's fatal cases, `doctor` must survive the broken
/// config it exists to diagnose.
fn effective_install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Vec<PackageManager> {
    // Detection order throughout; overrides only filter, never reorder.
    if let Some(o) = &overrides.pm {
        return ctx
            .package_managers
            .iter()
            .copied()
            .filter(|pm| *pm == o.pm)
            .collect();
    }
    if overrides.install_pms.is_empty() {
        return ctx.package_managers.clone();
    }
    ctx.package_managers
        .iter()
        .copied()
        .filter(|pm| overrides.install_pms.contains(pm))
        .collect()
}

/// What this invocation will install with, and what it decided about the
/// directories two package managers would otherwise write at once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InstallPlan {
    /// The package managers that actually run, in detection order.
    pub pms: Vec<PackageManager>,
    /// Writers dropped from `pms` because another writer owns their
    /// directory: `(loser, winner, dir)`.
    pub shadowed: Vec<(PackageManager, PackageManager, &'static str)>,
    /// Directories `pms` still writes with two or more managers, because the
    /// user asked for them all by name.
    pub collisions: Vec<DetectionWarning>,
    /// The `pms` sharing one directory, grouped, in the order they must run.
    /// Concurrent installs over one tree corrupt it, so these never race.
    pub serialized: Vec<Vec<PackageManager>>,
}

/// Resolve the install set and every install-directory collision in it.
///
/// Detection records which managers write which directory ([`ProjectContext::install_dirs`])
/// without judging it, because whether a shared directory is a collision
/// depends on the install set, which only overrides can settle. This is where
/// it gets settled, and it is the only place: nothing else in the codebase
/// decides what "colliding" means.
///
/// # Errors
///
/// [`ResolveError::InstallDirCollision`] under
/// [`CollisionPolicy::Error`], plus the not-detected errors from
/// [`select_install_pms`].
pub(crate) fn plan_install(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Result<InstallPlan, ResolveError> {
    let mut plan = InstallPlan {
        pms: select_install_pms(ctx, overrides)?,
        shadowed: Vec::new(),
        collisions: Vec::new(),
        serialized: Vec::new(),
    };

    for install_dir in &ctx.install_dirs {
        let writers: Vec<PackageManager> = install_dir
            .writers
            .iter()
            .copied()
            .filter(|pm| plan.pms.contains(pm))
            .collect();
        if writers.len() < 2 {
            continue;
        }
        if overrides.on_collision == CollisionPolicy::Error {
            return Err(ResolveError::InstallDirCollision {
                dir: install_dir.dir,
                writers,
            });
        }
        // Naming several writers in the allowlist is consent: the user knows
        // both write the tree and wants both to run. Runner honours it, warns
        // once, and serializes them so consent doesn't become corruption.
        if consented_to(&writers, overrides) {
            plan.collisions.push(DetectionWarning::InstallDirCollision {
                dir: install_dir.dir,
                pms: writers.clone(),
            });
            plan.serialized.push(writers);
            continue;
        }
        let winner = dir_winner(ctx, overrides, &writers);
        for loser in writers.iter().copied().filter(|pm| *pm != winner) {
            plan.shadowed.push((loser, winner, install_dir.dir));
            plan.pms.retain(|pm| *pm != loser);
        }
    }

    Ok(plan)
}

/// Whether the user named two or more of `writers` in the install allowlist,
/// rather than runner having swept them in by detecting everything.
fn consented_to(writers: &[PackageManager], overrides: &ResolutionOverrides) -> bool {
    writers
        .iter()
        .filter(|pm| overrides.install_pms.contains(pm))
        .count()
        >= 2
}

/// Which writer owns a shared install directory.
///
/// The node ecosystem already has an answer: the PM the resolver picks for
/// `package.json` scripts (lockfile, `packageManager`, `[pm].node`, PATH
/// probe). Reusing it keeps one project from having two different "primary"
/// package managers depending on whether it is running a script or an install,
/// and it means `[pm].node = "deno"` hands deno the tree without a second knob.
/// A directory whose writers the resolver has no opinion about falls back to
/// detection order.
fn dir_winner(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    writers: &[PackageManager],
) -> PackageManager {
    let resolved = Resolver::new(ctx, overrides)
        .resolve_node_pm()
        .ok()
        .map(|decision| decision.pm)
        .filter(|pm| writers.contains(pm));
    resolved.unwrap_or_else(|| {
        writers
            .first()
            .copied()
            .expect("dir_winner is only called with at least two writers")
    })
}

/// Print what the plan decided: the collisions the user asked for, then the
/// writers that lost a directory. A skipped install is never silent, that is
/// how a lockfile goes stale without anyone noticing.
fn report_plan(plan: &InstallPlan, overrides: &ResolutionOverrides) {
    if overrides.no_warnings {
        return;
    }
    for collision in &plan.collisions {
        eprintln!("{} {collision}", "warn:".yellow().bold());
    }
    for (loser, winner, dir) in &plan.shadowed {
        eprintln!(
            "{}",
            format!(
                "{dir}/: {} installs it, {} shadowed (run both with `[install].pms = [\"{}\", \
                 \"{}\"]`)",
                winner.label(),
                loser.label(),
                winner.label(),
                loser.label(),
            )
            .dimmed(),
        );
    }
}

/// Run a single PM's install in the foreground, inheriting stdio.
fn install_single(
    ctx: &ProjectContext,
    pm: PackageManager,
    frozen: bool,
    overrides: &ResolutionOverrides,
) -> Result<i32> {
    eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
    let mut cmd = build_install_command(ctx, pm, frozen, script_directive(overrides));
    super::configure_command(&mut cmd, &ctx.root, overrides);
    let status = cmd.status()?;
    Ok(if status.success() {
        0
    } else {
        super::exit_code(status)
    })
}

/// Split the plan into lanes: the package managers that share an install
/// directory form one lane and run in sequence, everything else gets a lane of
/// its own. Lanes are emitted in detection order, as are the managers inside
/// one.
fn install_lanes(plan: &InstallPlan) -> Vec<Vec<PackageManager>> {
    let mut lanes: Vec<Vec<PackageManager>> = Vec::new();
    for pm in &plan.pms {
        if lanes.iter().flatten().any(|queued| queued == pm) {
            continue;
        }
        match plan
            .serialized
            .iter()
            .find(|shared_dir| shared_dir.contains(pm))
        {
            Some(shared_dir) => lanes.push(shared_dir.clone()),
            None => lanes.push(vec![*pm]),
        }
    }
    lanes
}

/// Run the plan's lanes concurrently, multiplexing stdout/stderr through a
/// [`LineSink`] so each line is prefixed with the PM that produced it.
///
/// Managers sharing an install directory run in sequence inside their lane:
/// two installs writing one tree at the same time corrupt it.
///
/// Failure policy mirrors chain mode's `FailFast` default: record the first
/// non-zero exit code (by detection order), let the other lanes finish on their
/// own. A failure *inside* a lane stops that lane, because the next manager in
/// it would install over the tree the failed one left behind.
fn run_installs_parallel(
    ctx: &ProjectContext,
    plan: &InstallPlan,
    frozen: bool,
    overrides: &ResolutionOverrides,
) -> Result<i32> {
    let lanes = install_lanes(plan);
    let names: Vec<&str> = plan.pms.iter().map(|pm| pm.label()).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);
    let directive = script_directive(overrides);

    let outcomes: Vec<Result<Option<(PackageManager, i32)>>> = std::thread::scope(|scope| {
        // Every lane is spawned before any is joined: joining as we go would
        // serialize the lanes, which is the whole thing this function exists
        // not to do.
        let mut handles = Vec::with_capacity(lanes.len());
        for lane in &lanes {
            let sink = Arc::clone(&sink);
            handles.push(scope.spawn(move || {
                run_lane(
                    ctx, lane, frozen, overrides, &sink, width, colorize, directive,
                )
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|payload| {
                    bail!("install lane panicked: {}", panic_payload(&*payload))
                })
            })
            .collect()
    });

    let mut failures: Vec<(PackageManager, i32)> = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(Some(failure)) => failures.push(failure),
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }
    let index_of = |pm: PackageManager| plan.pms.iter().position(|p| *p == pm);
    Ok(failures
        .into_iter()
        .min_by_key(|(pm, _)| index_of(*pm))
        .map_or(0, |(_, code)| code))
}

/// Run one lane's installs back to back, returning the lane's first failure.
#[allow(
    clippy::too_many_arguments,
    reason = "one lane's worth of the parallel executor's state; bundling it into a struct buys \
              nothing at one call site"
)]
fn run_lane(
    ctx: &ProjectContext,
    lane: &[PackageManager],
    frozen: bool,
    overrides: &ResolutionOverrides,
    sink: &Arc<dyn LineSink>,
    width: usize,
    colorize: bool,
    directive: tool::ScriptDirective,
) -> Result<Option<(PackageManager, i32)>> {
    for pm in lane {
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
        let mut cmd = build_install_command(ctx, *pm, frozen, directive);
        super::configure_command(&mut cmd, &ctx.root, overrides);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let prefix = render_prefix(pm.label(), width, colorize);
        let stdout: Box<dyn std::io::Read + Send> =
            Box::new(child.stdout.take().expect("stdout piped"));
        let stderr: Box<dyn std::io::Read + Send> =
            Box::new(child.stderr.take().expect("stderr piped"));
        let readers = spawn_readers(
            vec![(prefix.clone(), false, stdout), (prefix, true, stderr)],
            sink,
        );

        let waited = child.wait();
        for handle in readers {
            join_reader_thread(handle);
        }
        let status = match waited {
            Ok(status) => status,
            Err(e) => {
                // The child outlives a failed `wait`; reap it here rather than
                // drop the handle and orphan the process.
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.into());
            }
        };
        if !status.success() {
            return Ok(Some((*pm, super::exit_code(status))));
        }
    }
    Ok(None)
}

fn join_reader_thread(handle: JoinHandle<()>) {
    if let Err(payload) = handle.join() {
        eprintln!(
            "warn: install output reader thread panicked: {}",
            panic_payload(&*payload),
        );
    }
}

fn panic_payload(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

/// Map a [`PackageManager`] to its install [`Command`].
///
/// `scripts` carries the resolved lifecycle-script policy. It is honored only
/// by managers that expose a mechanism: deny via npm/yarn/pnpm/bun
/// `--ignore-scripts`, composer `--no-scripts`, yarn-berry
/// `YARN_ENABLE_SCRIPTS=false`; force-on via npm `--no-ignore-scripts`,
/// yarn-berry `YARN_ENABLE_SCRIPTS=true`, deno `--allow-scripts`. Managers that
/// cannot honor a direction (deno deny / cargo·go·bundler·python deny;
/// bun·pnpm force-on) are warned about by the callers
/// ([`warn_unsupported_script_policy`]) before reaching here.
fn build_install_command(
    ctx: &ProjectContext,
    pm: PackageManager,
    frozen: bool,
    scripts: tool::ScriptDirective,
) -> Command {
    match pm {
        PackageManager::Npm => tool::npm::install_cmd(frozen, scripts),
        PackageManager::Yarn => tool::yarn::install_cmd(&ctx.root, frozen, scripts),
        PackageManager::Pnpm => tool::pnpm::install_cmd(frozen, scripts),
        PackageManager::Bun => tool::bun::install_cmd(frozen, scripts),
        PackageManager::Cargo => tool::cargo_pm::install_cmd(frozen),
        PackageManager::Deno => tool::deno::install_cmd(scripts),
        PackageManager::Uv => tool::uv::install_cmd(frozen),
        PackageManager::Poetry => tool::poetry::install_cmd(),
        PackageManager::Pipenv => tool::pipenv::install_cmd(frozen),
        PackageManager::Go => tool::go_pm::install_cmd(),
        PackageManager::Bundler => tool::bundler::install_cmd(),
        PackageManager::Composer => tool::composer::install_cmd(scripts),
    }
}

/// Lower the resolved [`ScriptPolicy`] to the per-tool [`tool::ScriptDirective`]
/// that [`build_install_command`] threads into each manager's `install_cmd`.
const fn script_directive(overrides: &ResolutionOverrides) -> tool::ScriptDirective {
    match overrides.script_policy {
        ScriptPolicy::Default => tool::ScriptDirective::Default,
        ScriptPolicy::Deny => tool::ScriptDirective::Deny,
        ScriptPolicy::Allow => tool::ScriptDirective::ForceOn,
    }
}

/// Whether the resolved [`ScriptPolicy`] asks to skip install scripts.
const fn deny_scripts(overrides: &ResolutionOverrides) -> bool {
    matches!(overrides.script_policy, ScriptPolicy::Deny)
}

/// Whether the resolved [`ScriptPolicy`] asks to force install scripts on.
const fn force_scripts(overrides: &ResolutionOverrides) -> bool {
    matches!(overrides.script_policy, ScriptPolicy::Allow)
}

/// How a package manager can honor an install-script *deny* policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenySupport {
    /// Skips scripts via a flag/env that [`build_install_command`] applies
    /// (npm/yarn/pnpm/bun `--ignore-scripts`, composer `--no-scripts`,
    /// yarn-berry `YARN_ENABLE_SCRIPTS=false`).
    ViaFlag,
    /// Already blocks dependency scripts by default (deno), so a deny
    /// request is satisfied without changing the command.
    DefaultDeny,
    /// No mechanism to skip install/build scripts (cargo `build.rs`, go,
    /// bundler native extensions, the Python build backends uv/poetry/pipenv).
    /// A deny request is reported and the install proceeds unchanged.
    Unsupported,
}

/// Classify how each package manager honors a deny-scripts policy. Exhaustive
/// over [`PackageManager`] so a newly added PM is a compile error to triage.
const fn deny_support(pm: PackageManager) -> DenySupport {
    match pm {
        PackageManager::Npm
        | PackageManager::Yarn
        | PackageManager::Pnpm
        | PackageManager::Bun
        | PackageManager::Composer => DenySupport::ViaFlag,
        PackageManager::Deno => DenySupport::DefaultDeny,
        PackageManager::Cargo
        | PackageManager::Uv
        | PackageManager::Poetry
        | PackageManager::Pipenv
        | PackageManager::Go
        | PackageManager::Bundler => DenySupport::Unsupported,
    }
}

/// How a package manager can honor a force-scripts-on policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForceSupport {
    /// Forces scripts on via a flag/env that [`build_install_command`] applies
    /// (npm `--no-ignore-scripts`, yarn-berry `YARN_ENABLE_SCRIPTS=true`, deno
    /// `--allow-scripts`).
    ViaFlag,
    /// Already runs install/build scripts by default, so force-on is satisfied
    /// without changing the command (composer, cargo, go, bundler, the Python
    /// backends uv/poetry/pipenv, and yarn-classic).
    AlreadyRuns,
    /// Denies dependency build scripts by default and re-enables them only
    /// through a manifest allowlist runner must not write (bun
    /// `trustedDependencies`, pnpm >=10 `onlyBuiltDependencies`), so force-on
    /// cannot be expressed by a flag. A force-on request is reported and the
    /// install proceeds at the manager's default.
    NotExpressible,
}

/// Classify how each package manager honors a force-scripts-on policy.
/// Exhaustive over [`PackageManager`] so a newly added PM is a compile error to
/// triage.
///
/// Yarn is [`ForceSupport::ViaFlag`]: Berry expresses force-on via
/// `YARN_ENABLE_SCRIPTS=true` while Classic runs scripts by default (a no-op);
/// the version split is resolved inside [`tool::yarn`], and neither path warns.
const fn force_support(pm: PackageManager) -> ForceSupport {
    match pm {
        PackageManager::Npm | PackageManager::Deno | PackageManager::Yarn => ForceSupport::ViaFlag,
        PackageManager::Bun | PackageManager::Pnpm => ForceSupport::NotExpressible,
        PackageManager::Composer
        | PackageManager::Cargo
        | PackageManager::Uv
        | PackageManager::Poetry
        | PackageManager::Pipenv
        | PackageManager::Go
        | PackageManager::Bundler => ForceSupport::AlreadyRuns,
    }
}

/// Warn once per selected package manager whose script policy cannot be
/// honored, so a `--no-scripts`/`--scripts` (or the `[install].scripts` /
/// `RUNNER_INSTALL_SCRIPTS` equivalents) that some managers ignore is never
/// silently dropped. No-op under [`ScriptPolicy::Default`]; otherwise emits one
/// line per affected manager.
///
/// Unlike the cosmetic collision/version warnings, both notices fire
/// unconditionally when their policy is active and are *not* silenced by
/// `--no-warnings` / `RUNNER_NO_WARNINGS`:
/// - **deny** is a security-relevant disclosure, the unsupported managers
///   (bundler, uv/poetry/pipenv, cargo, go) execute arbitrary install-time
///   code and have no flag to skip it, so a dropped deny must never hide.
/// - **force-on** is a request-fidelity disclosure, bun and pnpm (>=10) deny
///   dependency build scripts by default and only a manifest allowlist runner
///   won't write re-enables them, so the user learns their `--scripts` couldn't
///   be applied rather than assuming it was.
fn warn_unsupported_script_policy(pms: &[PackageManager], overrides: &ResolutionOverrides) {
    for pm in unsupported_deny_managers(pms, overrides) {
        eprintln!(
            "{} {} cannot skip install scripts; deny policy not applied to it",
            "warn:".yellow().bold(),
            pm.label(),
        );
    }
    for pm in unforceable_managers(pms, overrides) {
        let allowlist = match pm {
            PackageManager::Pnpm => "pnpm.onlyBuiltDependencies",
            PackageManager::Bun => "trustedDependencies",
            _ => "a manifest allowlist",
        };
        eprintln!(
            "{} {} cannot force install scripts on; it denies dependency build scripts by default \
             and only the {} allowlist (which runner won't write) re-enables them",
            "warn:".yellow().bold(),
            pm.label(),
            allowlist,
        );
    }
}

/// Selected managers that cannot honor an active deny-scripts policy, in
/// selection order. Empty unless the policy is [`ScriptPolicy::Deny`]. Drives
/// [`warn_unsupported_script_policy`]; pulled out so the security disclosure's
/// firing rule (fires whenever a deny is requested, independent of
/// `--no-warnings`) is unit-testable without capturing stderr.
fn unsupported_deny_managers(
    pms: &[PackageManager],
    overrides: &ResolutionOverrides,
) -> Vec<PackageManager> {
    if !deny_scripts(overrides) {
        return Vec::new();
    }
    pms.iter()
        .copied()
        .filter(|pm| deny_support(*pm) == DenySupport::Unsupported)
        .collect()
}

/// Selected managers whose force-scripts-on request cannot be expressed by a
/// flag, in selection order. Empty unless the policy is [`ScriptPolicy::Allow`].
/// Drives [`warn_unsupported_script_policy`]; pulled out so the firing rule
/// (fires whenever a force-on is requested, independent of `--no-warnings`) is
/// unit-testable without capturing stderr.
fn unforceable_managers(
    pms: &[PackageManager],
    overrides: &ResolutionOverrides,
) -> Vec<PackageManager> {
    if !force_scripts(overrides) {
        return Vec::new();
    }
    pms.iter()
        .copied()
        .filter(|pm| force_support(*pm) == ForceSupport::NotExpressible)
        .collect()
}

/// Print a hint about which version manager command to run.
fn suggest_version_switch(ctx: &ProjectContext) {
    let hint = if ctx
        .node_version
        .as_ref()
        .is_some_and(|nv| nv.source == ".nvmrc")
    {
        "nvm use"
    } else if ctx.task_runners.contains(&TaskRunner::Mise) {
        "mise install"
    } else {
        "switch to the expected Node version"
    };
    eprintln!("       {} {}", "hint:".dimmed(), hint);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::{
        DenySupport, ForceSupport, InstallPlan, build_install_command, deny_support, force_support,
        install_lanes, plan_install, script_directive, select_install_pms, unforceable_managers,
        unsupported_deny_managers, warn_unsupported_script_policy,
    };
    use crate::resolver::{
        CollisionPolicy, OverrideOrigin, PmOverride, ResolutionOverrides, ResolveError,
        ScriptPolicy,
    };
    use crate::tool::ScriptDirective;
    use crate::types::{DetectionWarning, Ecosystem, InstallDir, PackageManager, ProjectContext};

    fn context(pms: Vec<PackageManager>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("/tmp/test"),
            package_managers: pms,
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn override_pm(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..Default::default()
        }
    }

    #[test]
    fn no_override_installs_with_every_detected_pm() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let pms = select_install_pms(&ctx, &ResolutionOverrides::default())
            .expect("default selection should succeed");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Deno]);
    }

    #[test]
    fn detected_override_installs_with_it_alone() {
        // The dreamcli CI bug: bun + deno detected, RUNNER_PM=bun set,
        // deno must not install (and must not write deno.lock).
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let overrides = override_pm(PackageManager::Bun, OverrideOrigin::EnvVar);
        let pms = select_install_pms(&ctx, &overrides).expect("detected override should filter");
        assert_eq!(pms, vec![PackageManager::Bun]);
    }

    #[test]
    fn undetected_override_errors_with_origin_and_detected_list() {
        let ctx = context(vec![PackageManager::Cargo]);
        let overrides = override_pm(PackageManager::Npm, OverrideOrigin::EnvVar);
        let err = select_install_pms(&ctx, &overrides).expect_err("undetected override must error");

        assert!(matches!(err, ResolveError::PmOverrideNotDetected { .. }));
        let msg = format!("{err}");
        assert!(msg.contains("RUNNER_PM"), "should name the source: {msg}");
        assert!(msg.contains("cargo"), "should list detected PMs: {msg}");
    }

    #[test]
    fn undetected_cli_override_names_the_flag() {
        let ctx = context(vec![PackageManager::Cargo]);
        let overrides = override_pm(PackageManager::Npm, OverrideOrigin::CliFlag);
        let err = select_install_pms(&ctx, &overrides).expect_err("undetected override must error");

        let msg = format!("{err}");
        assert!(msg.contains("--pm"), "should name the flag: {msg}");
    }

    #[test]
    fn install_pms_allowlist_filters_to_listed_detected_pms() {
        // The reported case: bun + deno + cargo detected, allowlist = [bun]
        // → only bun installs (no competing node_modules writer, no cargo).
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Deno,
            PackageManager::Cargo,
        ]);
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Bun],
            ..Default::default()
        };
        let pms = select_install_pms(&ctx, &overrides).expect("allowlist should filter");
        assert_eq!(pms, vec![PackageManager::Bun]);
    }

    /// bun + deno both writing `node_modules`, which is dreamcli's shape.
    fn colliding_context() -> ProjectContext {
        let mut ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        ctx.install_dirs = vec![InstallDir {
            dir: "node_modules",
            writers: vec![PackageManager::Bun, PackageManager::Deno],
        }];
        ctx
    }

    #[test]
    fn colliding_writers_resolve_to_one_installer_by_default() {
        let ctx = colliding_context();
        let plan = plan_install(&ctx, &ResolutionOverrides::default()).expect("plan");

        assert_eq!(plan.pms, vec![PackageManager::Bun], "deno must not install");
        assert_eq!(
            plan.shadowed,
            vec![(PackageManager::Deno, PackageManager::Bun, "node_modules")],
        );
        assert!(plan.collisions.is_empty(), "resolved, so nothing to warn");
        assert!(plan.serialized.is_empty());
    }

    #[test]
    fn ecosystem_pm_override_hands_the_tree_to_deno() {
        // `[pm].node = "deno"` picks deno for package.json scripts; the same
        // decision decides who owns node_modules, so bun is the one shadowed.
        let ctx = colliding_context();
        let mut overrides = ResolutionOverrides::default();
        overrides.pm_by_ecosystem.insert(
            Ecosystem::Deno,
            PmOverride {
                pm: PackageManager::Deno,
                origin: OverrideOrigin::ConfigFile {
                    path: PathBuf::from("/tmp/test/runner.toml"),
                },
            },
        );

        let plan = plan_install(&ctx, &overrides).expect("plan");

        assert_eq!(plan.pms, vec![PackageManager::Deno]);
        assert_eq!(
            plan.shadowed,
            vec![(PackageManager::Bun, PackageManager::Deno, "node_modules")],
        );
    }

    #[test]
    fn naming_both_writers_runs_both_serialized_with_one_warning() {
        let ctx = colliding_context();
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Bun, PackageManager::Deno],
            ..Default::default()
        };

        let plan = plan_install(&ctx, &overrides).expect("plan");

        assert_eq!(plan.pms, vec![PackageManager::Bun, PackageManager::Deno]);
        assert!(plan.shadowed.is_empty(), "consent means nothing is dropped");
        assert!(matches!(
            plan.collisions.as_slice(),
            [DetectionWarning::InstallDirCollision { dir, pms }]
                if *dir == "node_modules"
                    && pms == &[PackageManager::Bun, PackageManager::Deno]
        ));
        assert_eq!(
            plan.serialized,
            vec![vec![PackageManager::Bun, PackageManager::Deno]],
            "consented writers still must not race over one tree",
        );
    }

    #[test]
    fn on_collision_error_refuses_to_pick() {
        let ctx = colliding_context();
        let overrides = ResolutionOverrides {
            on_collision: CollisionPolicy::Error,
            ..Default::default()
        };

        let err = plan_install(&ctx, &overrides).expect_err("must refuse");

        assert!(matches!(err, ResolveError::InstallDirCollision { .. }));
        let msg = format!("{err}");
        assert!(msg.contains("node_modules"), "msg: {msg}");
        assert!(msg.contains("[install].pms"), "msg: {msg}");
    }

    #[test]
    fn on_collision_error_refuses_even_when_both_were_named() {
        // The strict CI guard means "never two writers on one tree", so an
        // explicit allowlist doesn't buy its way past it.
        let ctx = colliding_context();
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Bun, PackageManager::Deno],
            on_collision: CollisionPolicy::Error,
            ..Default::default()
        };

        let err = plan_install(&ctx, &overrides).expect_err("must refuse");
        assert!(matches!(err, ResolveError::InstallDirCollision { .. }));
    }

    #[test]
    fn pm_override_leaves_one_writer_and_nothing_to_say() {
        let ctx = colliding_context();
        let overrides = override_pm(PackageManager::Bun, OverrideOrigin::EnvVar);

        let plan = plan_install(&ctx, &overrides).expect("plan");

        assert_eq!(plan.pms, vec![PackageManager::Bun]);
        assert!(plan.collisions.is_empty());
        assert!(
            plan.shadowed.is_empty(),
            "nothing was dropped: deno was never in the install set to begin with"
        );
    }

    #[test]
    fn a_lone_writer_plans_clean() {
        let mut ctx = context(vec![PackageManager::Bun, PackageManager::Cargo]);
        ctx.install_dirs = vec![InstallDir {
            dir: "node_modules",
            writers: vec![PackageManager::Bun],
        }];

        let plan = plan_install(&ctx, &ResolutionOverrides::default()).expect("plan");

        assert_eq!(plan.pms, vec![PackageManager::Bun, PackageManager::Cargo]);
        assert!(plan.collisions.is_empty());
        assert!(plan.shadowed.is_empty());
        assert!(plan.serialized.is_empty());
    }

    #[test]
    fn lanes_serialize_shared_writers_and_leave_the_rest_parallel() {
        let plan = InstallPlan {
            pms: vec![
                PackageManager::Bun,
                PackageManager::Cargo,
                PackageManager::Deno,
            ],
            shadowed: Vec::new(),
            collisions: Vec::new(),
            serialized: vec![vec![PackageManager::Bun, PackageManager::Deno]],
        };

        assert_eq!(
            install_lanes(&plan),
            vec![
                vec![PackageManager::Bun, PackageManager::Deno],
                vec![PackageManager::Cargo],
            ],
            "bun and deno share one lane and run in order; cargo runs alongside",
        );
    }

    #[test]
    fn install_pms_allowlist_preserves_detection_order() {
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Cargo,
            PackageManager::Uv,
        ]);
        // Listed out of detection order, output still follows detection.
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Uv, PackageManager::Bun],
            ..Default::default()
        };
        let pms = select_install_pms(&ctx, &overrides).expect("allowlist should filter");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Uv]);
    }

    #[test]
    fn install_pms_undetected_entry_errors() {
        let ctx = context(vec![PackageManager::Bun]);
        let overrides = ResolutionOverrides {
            install_pms: vec![PackageManager::Bun, PackageManager::Pnpm],
            ..Default::default()
        };
        let err = select_install_pms(&ctx, &overrides).expect_err("undetected entry must error");
        assert!(matches!(err, ResolveError::InstallPmsNotDetected { .. }));
        let msg = format!("{err}");
        assert!(msg.contains("pnpm"), "names the missing PM: {msg}");
        assert!(msg.contains("bun"), "lists detected: {msg}");
    }

    #[test]
    fn pm_override_wins_over_install_pms_allowlist() {
        // --pm/RUNNER_PM is the cross-ecosystem override; it takes
        // precedence and the install allowlist is not consulted.
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let mut overrides = override_pm(PackageManager::Deno, OverrideOrigin::EnvVar);
        overrides.install_pms = vec![PackageManager::Bun];
        let pms = select_install_pms(&ctx, &overrides).expect("override wins");
        assert_eq!(pms, vec![PackageManager::Deno]);
    }

    #[test]
    fn empty_install_pms_installs_with_every_detected_pm() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Cargo]);
        let pms = select_install_pms(&ctx, &ResolutionOverrides::default())
            .expect("no allowlist installs all");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Cargo]);
    }

    #[test]
    fn ecosystem_config_override_does_not_filter_installs() {
        // Pins the documented non-goal: `[pm].node` in runner.toml scopes
        // script dispatch, not the install set.
        let ctx = context(vec![PackageManager::Bun, PackageManager::Cargo]);
        let mut pm_by_ecosystem = HashMap::new();
        pm_by_ecosystem.insert(
            Ecosystem::Node,
            PmOverride {
                pm: PackageManager::Pnpm,
                origin: OverrideOrigin::ConfigFile {
                    path: PathBuf::from("/tmp/test/runner.toml"),
                },
            },
        );
        let overrides = ResolutionOverrides {
            pm_by_ecosystem,
            ..Default::default()
        };

        let pms = select_install_pms(&ctx, &overrides).expect("config must not filter installs");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Cargo]);
    }

    /// argv produced by `build_install_command` for `pm`, ignoring the
    /// subprocess-probing managers (yarn) whose flag logic is unit-tested in
    /// their own module.
    fn install_argv(pm: PackageManager, scripts: ScriptDirective) -> Vec<String> {
        let ctx = context(vec![pm]);
        build_install_command(&ctx, pm, false, scripts)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn deny_support_classifies_every_pm() {
        for pm in [
            PackageManager::Npm,
            PackageManager::Yarn,
            PackageManager::Pnpm,
            PackageManager::Bun,
            PackageManager::Composer,
        ] {
            assert_eq!(
                deny_support(pm),
                DenySupport::ViaFlag,
                "{} via flag",
                pm.label()
            );
        }
        assert_eq!(deny_support(PackageManager::Deno), DenySupport::DefaultDeny);
        for pm in [
            PackageManager::Cargo,
            PackageManager::Uv,
            PackageManager::Poetry,
            PackageManager::Pipenv,
            PackageManager::Go,
            PackageManager::Bundler,
        ] {
            assert_eq!(
                deny_support(pm),
                DenySupport::Unsupported,
                "{} unsupported",
                pm.label(),
            );
        }
    }

    #[test]
    fn force_support_classifies_every_pm() {
        for pm in [
            PackageManager::Npm,
            PackageManager::Deno,
            PackageManager::Yarn,
        ] {
            assert_eq!(
                force_support(pm),
                ForceSupport::ViaFlag,
                "{} via flag",
                pm.label(),
            );
        }
        for pm in [PackageManager::Bun, PackageManager::Pnpm] {
            assert_eq!(
                force_support(pm),
                ForceSupport::NotExpressible,
                "{} not expressible",
                pm.label(),
            );
        }
        for pm in [
            PackageManager::Composer,
            PackageManager::Cargo,
            PackageManager::Uv,
            PackageManager::Poetry,
            PackageManager::Pipenv,
            PackageManager::Go,
            PackageManager::Bundler,
        ] {
            assert_eq!(
                force_support(pm),
                ForceSupport::AlreadyRuns,
                "{} already runs",
                pm.label(),
            );
        }
    }

    #[test]
    fn script_directive_lowers_every_policy() {
        let directive = |policy| {
            script_directive(&ResolutionOverrides {
                script_policy: policy,
                ..ResolutionOverrides::default()
            })
        };
        assert_eq!(directive(ScriptPolicy::Default), ScriptDirective::Default);
        assert_eq!(directive(ScriptPolicy::Deny), ScriptDirective::Deny);
        assert_eq!(directive(ScriptPolicy::Allow), ScriptDirective::ForceOn);
    }

    #[test]
    fn deny_appends_skip_flag_for_flag_managers() {
        assert_eq!(
            install_argv(PackageManager::Npm, ScriptDirective::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Pnpm, ScriptDirective::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Bun, ScriptDirective::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Composer, ScriptDirective::Deny),
            ["install", "--no-scripts"]
        );
    }

    #[test]
    fn deny_is_noop_for_default_deny_and_unsupported_managers() {
        // deno already denies by default, no flag added.
        assert_eq!(
            install_argv(PackageManager::Deno, ScriptDirective::Deny),
            ["install"]
        );
        // cargo has no toggle, the deny is reported elsewhere, command unchanged.
        assert_eq!(
            install_argv(PackageManager::Cargo, ScriptDirective::Deny),
            ["fetch"]
        );
    }

    #[test]
    fn force_on_appends_flag_for_flag_managers() {
        // npm negates ignore-scripts; deno allows all via bare --allow-scripts.
        assert_eq!(
            install_argv(PackageManager::Npm, ScriptDirective::ForceOn),
            ["install", "--no-ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Deno, ScriptDirective::ForceOn),
            ["install", "--allow-scripts"]
        );
    }

    #[test]
    fn force_on_is_noop_for_already_runs_and_unforceable_managers() {
        // composer/cargo run scripts by default, force-on changes nothing.
        assert_eq!(
            install_argv(PackageManager::Composer, ScriptDirective::ForceOn),
            ["install"]
        );
        assert_eq!(
            install_argv(PackageManager::Cargo, ScriptDirective::ForceOn),
            ["fetch"]
        );
        // bun/pnpm gate dependency builds behind a manifest allowlist, no flag;
        // the request is disclosed by warn_unsupported_script_policy instead.
        assert_eq!(
            install_argv(PackageManager::Bun, ScriptDirective::ForceOn),
            ["install"]
        );
        assert_eq!(
            install_argv(PackageManager::Pnpm, ScriptDirective::ForceOn),
            ["install"]
        );
    }

    #[test]
    fn default_adds_no_skip_flag() {
        assert_eq!(
            install_argv(PackageManager::Npm, ScriptDirective::Default),
            ["install"]
        );
        assert_eq!(
            install_argv(PackageManager::Composer, ScriptDirective::Default),
            ["install"]
        );
    }

    #[test]
    fn deny_disclosure_fires_for_unsupported_pms_regardless_of_no_warnings() {
        let pms = [PackageManager::Cargo, PackageManager::Npm];
        // Non-deny policy: nothing to disclose.
        assert!(unsupported_deny_managers(&pms, &ResolutionOverrides::default()).is_empty());
        // Deny + --no-warnings: the unsupported PM (cargo) is still disclosed,
        // because this is a security notice, not a cosmetic warning. Npm honors
        // the deny via a flag, so it is not listed.
        let denying = ResolutionOverrides {
            script_policy: ScriptPolicy::Deny,
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            unsupported_deny_managers(&pms, &denying),
            vec![PackageManager::Cargo]
        );
        // Smoke the public entry point with the same inputs: still emits, no panic.
        warn_unsupported_script_policy(&pms, &denying);
    }

    #[test]
    fn force_disclosure_fires_for_unforceable_pms_regardless_of_no_warnings() {
        let pms = [
            PackageManager::Pnpm,
            PackageManager::Npm,
            PackageManager::Bun,
        ];
        // Non-force policy: nothing to disclose.
        assert!(unforceable_managers(&pms, &ResolutionOverrides::default()).is_empty());
        // Force-on + --no-warnings: pnpm and bun (manifest-allowlist managers)
        // are still disclosed, in selection order. npm expresses force-on via a
        // flag, so it is not listed.
        let forcing = ResolutionOverrides {
            script_policy: ScriptPolicy::Allow,
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            unforceable_managers(&pms, &forcing),
            vec![PackageManager::Pnpm, PackageManager::Bun]
        );
        // Smoke the public entry point with the same inputs: still emits, no panic.
        warn_unsupported_script_policy(&pms, &forcing);
    }
}
