//! `runner install`, install dependencies via every detected package manager.

use std::any::Any;
use std::ffi::OsStr;
use std::process::Stdio;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use colored::Colorize;
use runner_core::Provider;
use runner_providers::REGISTRY;

use crate::chain::mux::{
    Delivery, LineSink, StdioSink, prefix_width, render_prefix, spawn_readers,
};
use crate::provider::Named;
use crate::resolver::{
    CollisionPolicy, LockfilePolicy, ResolutionOverrides, ResolveError, ScriptPolicy,
};

/// The install-scoped CLI flags, which have no env or config layer: they say
/// what this one invocation does, not what the project is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InstallFlags {
    /// `--frozen`: use each package manager's lockfile-only install variant.
    pub frozen: bool,
    /// `--no-tools`: skip the toolchain step that otherwise precedes the
    /// package managers.
    pub no_tools: bool,
}
use crate::tool;
use crate::types::{ProjectContext, version_matches};
use runner_core::ProviderId;

/// Install dependencies for each detected package manager.
///
/// Warns when an installed runtime falls outside the version the project
/// declares before proceeding. Thin wrapper over [`install_pms`]
/// that preserves the package manager's actual exit code for callers.
pub(crate) fn install(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    flags: InstallFlags,
) -> Result<i32> {
    install_pms(ctx, overrides, flags, None)
}

/// Chain-aware install entry. Runs install across every detected PM and
/// returns the first failing PM's exit code, or 0 if all succeed.
///
/// Used by `chain::exec` when `ChainItemKind::Install` appears as a
/// chain item (i.e. `runner install <tasks>`).
pub(crate) fn install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    flags: InstallFlags,
    mut sink: super::WarningSink<'_>,
) -> Result<i32> {
    let declared_tools = tools_step(
        ctx,
        &ResolutionOverrides::default(),
        InstallFlags::default(),
    );
    let tools = tools_step(ctx, overrides, flags);
    let task = install_task(ctx);
    // Planned before the GHA group opens so a refused override doesn't
    // emit an empty `runner: install` group.
    let plan = match plan_install(ctx, overrides) {
        Ok(plan) => plan,
        Err(ResolveError::NoInstallers) if declared_tools.is_some() || task.is_some() => {
            InstallPlan::empty()
        }
        Err(err) => return Err(err.into()),
    };
    if !(plan.pms.is_empty() && task.is_some()) {
        super::print_warnings(ctx, overrides, sink.as_deref_mut());
    }

    let mut execution = InstallExecution::new(
        ctx,
        overrides,
        flags.frozen || overrides.lockfile == LockfilePolicy::Frozen,
    )?;
    report_plan(&plan, overrides);
    disclose_script_clamps(&execution, &plan.pms, overrides)?;
    if !plan.pms.is_empty() || tools.is_some() {
        super::authorize_fetch(overrides, "install", "install")?;
    }

    // Collapse the whole install (single- or multi-PM) under one
    // `runner: install` GitHub Actions group when enabled.
    let group = super::task_group(overrides, "install", "install");

    if let Some(runner) = tools
        && let Some(code) = run_tools_step(&execution, runner, overrides)?
    {
        return Ok(code);
    }
    if tools.is_some() && !overrides.explain {
        execution.project.refresh_bins(&execution.tree, &REGISTRY);
    }
    if plan.pms.is_empty() {
        if let Some(task) = task {
            drop(group);
            return super::run::run(
                ctx,
                overrides,
                &format!("{}:{}", task.source.label(), task.name),
                &[],
                sink,
            );
        }
        return Ok(0);
    }

    if overrides.shows_warnings() {
        for runtime in ctx.runtime_versions() {
            if let (Some(expected), Some(cur)) = (&runtime.expected, &runtime.current)
                && !version_matches(&expected.version, cur)
            {
                eprintln!(
                    "{} {} expected {} ({}), current {cur}",
                    "warn:".yellow().bold(),
                    runtime.runtime.label(),
                    expected.version,
                    expected.source,
                );
            }
        }
    }

    if let [pm] = plan.pms.as_slice() {
        return install_single(&execution, *pm, overrides);
    }

    run_installs_parallel(&execution, &plan, overrides)
}

/// The tool manager that installs the project's toolchain before any
/// package manager runs: mise, when a mise config is detected and
/// `--no-tools` was not passed. Package managers themselves are often
/// mise-managed tools, so this step always precedes them.
pub(crate) fn tools_step(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    flags: InstallFlags,
) -> Option<ProviderId> {
    ctx.task_runners().into_iter().find(|runner| {
        let descriptor = provider(runner.label());
        !flags.no_tools
            && overrides
                .tool_install
                .get(descriptor.label)
                .is_none_or(|ops| !ops.is_empty())
            && descriptor.kind.contains(runner_core::Kind::TOOL_MANAGER)
            && !descriptor.caps.operations.is_empty()
    })
}

/// The project's own `install` task, run when no package manager has
/// anything to install: the current workspace member's, else the root's.
/// `runner why install` already names it.
fn install_task(ctx: &ProjectContext) -> Option<&crate::types::Task> {
    ctx.tasks
        .iter()
        .filter(|task| task.name == "install" && ctx.is_local(task))
        .min_by_key(|task| ctx.scope_rank(task))
}

/// Run the toolchain step in the foreground. Returns the tool manager's
/// exit code when it fails, so the package managers that may depend on the
/// tools it installs never run against a half-installed toolchain. A
/// missing binary is a warning: the config describes tools the host may
/// already have on `PATH`.
fn run_tools_step(
    execution: &InstallExecution,
    runner: ProviderId,
    overrides: &ResolutionOverrides,
) -> Result<Option<i32>> {
    for operation in tool_operations(runner, overrides, &execution.project)? {
        if let Some(code) = run_tool_operation(execution, runner, &operation, overrides)? {
            return Ok(Some(code));
        }
    }
    Ok(None)
}

/// Which operations `runner install` runs for `runner`, from
/// `[tools.<name>].install`, defaulting to the tool's install operation.
fn tool_operations(
    runner: ProviderId,
    overrides: &ResolutionOverrides,
    project: &runner_core::Project,
) -> Result<Vec<String>> {
    let operations = REGISTRY
        .effective(
            provider(runner.label()).id,
            project,
            &runner_core::Scope::Root,
        )
        .caps
        .operations;
    let Some(default) = operations.first() else {
        bail!("{} has no toolchain install step", runner.label());
    };
    let Some(configured) = overrides.tool_install.get(runner.label()) else {
        return Ok(vec![(*default).to_string()]);
    };
    for operation in configured {
        if !operations.contains(&operation.as_str()) {
            bail!(
                "[tools.{}].install: unknown operation {operation:?}; expected one of {}",
                runner.label(),
                operations.join(", "),
            );
        }
    }
    Ok(configured.clone())
}

/// The registry entry for a package manager or task runner label.
fn provider(label: &str) -> &'static Provider {
    REGISTRY
        .by_label(label)
        .unwrap_or_else(|| panic!("registry has no entry for {label}"))
}

struct InstallExecution {
    tree: runner_core::Tree,
    project: runner_core::Project,
    policy: runner_core::Policy,
}

impl InstallExecution {
    fn new(ctx: &ProjectContext, overrides: &ResolutionOverrides, frozen: bool) -> Result<Self> {
        let mut policy = super::run::core::policy(overrides);
        policy.frozen = frozen;
        policy.scripts = match overrides.script_policy {
            ScriptPolicy::Default => runner_core::ScriptPolicy::Default,
            ScriptPolicy::Deny => runner_core::ScriptPolicy::Deny,
            ScriptPolicy::Allow => runner_core::ScriptPolicy::Allow,
        };
        super::run::core::apply_host_verbosity(&mut policy, overrides, "install");
        let project = super::run::core::project(ctx)?;
        Ok(Self {
            tree: super::run::core::tree(ctx),
            project,
            policy,
        })
    }

    fn plan(
        &self,
        label: &str,
        operations: &[String],
        overrides: &ResolutionOverrides,
    ) -> Result<runner_core::Plan> {
        let provider = provider(label);
        let present = self
            .project
            .present_in(provider.id, &runner_core::Scope::Root)
            .ok_or_else(|| anyhow::anyhow!("no evidence for install provider {label}"))?;
        let mut plan = runner_core::plan_with(
            &self.tree,
            &self.project,
            &self.policy,
            present,
            &runner_core::Op::Install { operations },
            &REGISTRY,
        )?;
        super::configure_plan(&mut plan, overrides, "install");
        Ok(plan)
    }
}

/// Run one of the tool manager's operations in the foreground.
fn run_tool_operation(
    execution: &InstallExecution,
    runner: ProviderId,
    operation: &str,
    overrides: &ResolutionOverrides,
) -> Result<Option<i32>> {
    if overrides.shows_progress() {
        eprintln!(
            "{} {} {}",
            "running".dimmed(),
            runner.label().bold(),
            operation.bold()
        );
    }
    let plan = execution.plan(runner.label(), &[operation.to_owned()], overrides)?;
    let mut cmd = runner_core::execute::command(&plan)?;
    super::configure_task_streams(&mut cmd, overrides, "install");
    if overrides.explain {
        crate::render::explain::print_plan(overrides, &plan);
        crate::render::explain::print_command(overrides, &cmd);
        return Ok(None);
    }
    let mut child = match runner_core::execute::spawn(&plan, &mut cmd) {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if overrides.shows_warnings() {
                eprintln!(
                    "{} {} config detected but `{}` is not on PATH; skipping the toolchain step \
                     (pass --no-tools to silence this)",
                    "warn:".yellow().bold(),
                    runner.label(),
                    cmd.get_program().to_string_lossy(),
                );
            }
            return Ok(None);
        }
        Err(error) => {
            let program = cmd.get_program().to_string_lossy().into_owned();
            return Err(anyhow::Error::new(error).context(format!(
                "installing tools with {}: `{program}` failed to launch",
                runner.label()
            )));
        }
    };
    let status = child.wait()?;
    if status.success() {
        return Ok(None);
    }
    Ok(Some(super::exit_code(status)))
}

/// Select installers using package-manager choices and per-tool install policy.
fn select_installers(
    detected: &[ProviderId],
    overrides: &ResolutionOverrides,
) -> Result<Vec<ProviderId>, ResolveError> {
    if let Some(o) = &overrides.pm
        && !detected.contains(&o.pm)
    {
        return Err(ResolveError::PmOverrideNotDetected {
            pm: o.pm,
            origin: o.origin.clone(),
            detected: detected.to_vec(),
        });
    }

    if overrides.pm.is_none() {
        for choice in overrides.pm_by_ecosystem.values() {
            if !detected.contains(&choice.pm) {
                return Err(ResolveError::PmOverrideNotDetected {
                    pm: choice.pm,
                    origin: choice.origin.clone(),
                    detected: detected.to_vec(),
                });
            }
        }
    }
    Ok(effective_install_pms(detected, overrides))
}

/// The package managers with an install capability in the root scope, a
/// probed one only when it dispatches a task source observed there.
fn root_installers(project: &runner_core::Project) -> Vec<ProviderId> {
    let observed = |present: &runner_core::Present| {
        present
            .because
            .iter()
            .any(|evidence| evidence.weight < runner_core::Weight::Probed)
    };
    let dispatches_a_source = |present: &runner_core::Present| {
        let sources = REGISTRY
            .by_id(present.provider)
            .for_present(present)
            .caps
            .run_task
            .map_or(&[][..], |cap| cap.sources);
        project.present.iter().any(|source| {
            source.scope == runner_core::Scope::Root
                && observed(source)
                && sources.contains(&source.provider)
        })
    };
    let mut installers: Vec<_> = project
        .present
        .iter()
        .filter(|present| {
            present.scope == runner_core::Scope::Root
                && (observed(present) || dispatches_a_source(present))
                && REGISTRY
                    .by_id(present.provider)
                    .kind
                    .contains(runner_core::Kind::PACKAGE_MANAGER)
                && REGISTRY
                    .by_id(present.provider)
                    .for_present(present)
                    .caps
                    .install
                    .is_some()
        })
        .filter_map(|present| {
            crate::provider::package_manager(REGISTRY.by_id(present.provider).label)
        })
        .collect();
    installers.dedup();
    installers
}

/// The same selection as [`select_installers`] with the not-detected
/// errors dropped: an override naming an absent PM narrows to nothing
/// instead of failing. Reporting surfaces need the effective set without
/// inheriting dispatch's fatal cases; `doctor` must survive the broken
/// config it exists to diagnose.
fn effective_install_pms(
    detected: &[ProviderId],
    overrides: &ResolutionOverrides,
) -> Vec<ProviderId> {
    // Detection order throughout; overrides only filter, never reorder.
    if let Some(o) = &overrides.pm {
        return detected
            .iter()
            .copied()
            .filter(|pm| *pm == o.pm)
            .filter(|pm| {
                overrides
                    .tool_install
                    .get(pm.label())
                    .is_none_or(|ops| !ops.is_empty())
            })
            .collect();
    }
    detected
        .iter()
        .copied()
        .filter(|pm| {
            overrides
                .pm_by_ecosystem
                .get(&pm.ecosystem())
                .is_none_or(|choice| choice.pm == *pm)
        })
        .filter(|pm| {
            overrides
                .tool_install
                .get(pm.label())
                .is_none_or(|ops| !ops.is_empty())
        })
        .collect()
}

/// A directory the install set still writes with two or more managers, because
/// the user named them all. Its writers run in sequence, since concurrent
/// installs over one tree corrupt it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CollisionDir {
    pub dir: &'static str,
    pub writers: Vec<ProviderId>,
}

/// A writer dropped from the install set because another manager owns the
/// directory it would have written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shadowed {
    pub loser: ProviderId,
    pub winner: ProviderId,
    pub dir: &'static str,
}

/// What this invocation will install with, and what it decided about the
/// directories two package managers would otherwise write at once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InstallPlan {
    /// The package managers that actually run, in detection order.
    pub pms: Vec<ProviderId>,
    /// Writers dropped because another writer owns their directory.
    pub shadowed: Vec<Shadowed>,
    /// Directories kept with two or more writers on the user's say-so. The
    /// warning text and the serial run-order both derive from this.
    pub collisions: Vec<CollisionDir>,
}

impl InstallPlan {
    /// A plan with no package managers: the toolchain step alone.
    const fn empty() -> Self {
        Self {
            pms: Vec::new(),
            shadowed: Vec::new(),
            collisions: Vec::new(),
        }
    }
}

/// Resolve the install set and every install-directory collision in it.
///
/// Whether a shared directory is a collision depends on the install set,
/// which only overrides can settle. This is where it gets settled, and it is
/// the only place: nothing else in the codebase decides what "colliding"
/// means.
///
/// # Errors
///
/// [`ResolveError::InstallDirCollision`] under
/// [`CollisionPolicy::Error`], plus the not-detected errors from
/// [`select_installers`].
///
/// The install set is the package managers the core resolver finds in the
/// root scope.
pub(crate) fn plan_install(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Result<InstallPlan, ResolveError> {
    let project = super::run::core::project(ctx).map_err(ResolveError::Observation)?;
    let detected = root_installers(&project);
    if detected.is_empty() && overrides.pm.is_none() && overrides.pm_by_ecosystem.is_empty() {
        return Err(ResolveError::NoInstallers);
    }
    let mut plan = InstallPlan {
        pms: select_installers(&detected, overrides)?,
        shadowed: Vec::new(),
        collisions: Vec::new(),
    };

    for install_dir in &install_dirs(ctx, &detected) {
        let writers: Vec<ProviderId> = install_dir
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
        if consented_to(&writers, overrides) {
            plan.collisions.push(CollisionDir {
                dir: install_dir.dir,
                writers,
            });
            continue;
        }
        let winner = dir_winner(ctx, overrides, &writers)?;
        for loser in writers.iter().copied().filter(|pm| *pm != winner) {
            plan.shadowed.push(Shadowed {
                loser,
                winner,
                dir: install_dir.dir,
            });
            plan.pms.retain(|pm| *pm != loser);
        }
    }

    Ok(plan)
}

/// One install directory and every installer that writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallDir {
    /// Path relative to the project root, e.g. `"node_modules"`.
    pub dir: &'static str,
    /// Writers in install-set order.
    pub writers: Vec<ProviderId>,
}

/// The install directories `pms` write at the root, from each provider's
/// declared `writes` under its observed variant.
fn install_dirs(ctx: &ProjectContext, pms: &[ProviderId]) -> Vec<InstallDir> {
    let mut dirs: Vec<InstallDir> = Vec::new();
    for pm in pms {
        let provider = REGISTRY.by_id(*pm);
        let provider = ctx
            .project
            .as_ref()
            .ok()
            .and_then(|project| project.present_in(*pm, &runner_core::Scope::Root))
            .map_or(*provider, |present| provider.for_present(present));
        for written in provider.caps.writes {
            match dirs.iter_mut().find(|entry| entry.dir == *written) {
                Some(entry) => entry.writers.push(*pm),
                None => dirs.push(InstallDir {
                    dir: written,
                    writers: vec![*pm],
                }),
            }
        }
    }
    dirs
}

/// The warning shown when the install set keeps two or more writers on one
/// directory. They run serially, so nothing corrupts, but the redundant second
/// install is worth flagging.
pub(crate) fn collision_warning(dir: &str, writers: &[ProviderId]) -> String {
    let list = writers
        .iter()
        .map(|pm| pm.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{list} all install into {dir}/ and have explicit install operations; they run one after \
         another. Set [tools.<name>].install = false to disable an installer."
    )
}

/// Whether the user enabled every writer explicitly.
fn consented_to(writers: &[ProviderId], overrides: &ResolutionOverrides) -> bool {
    writers
        .iter()
        .filter(|pm| {
            overrides
                .tool_install
                .get(pm.label())
                .is_some_and(|ops| !ops.is_empty())
        })
        .count()
        == writers.len()
}

/// Rank writers using the resolved provider evidence and policy choices.
fn dir_winner(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    writers: &[ProviderId],
) -> Result<ProviderId, ResolveError> {
    let policy = super::run::core::policy(overrides);
    let project = super::run::core::project(ctx).map_err(ResolveError::Observation)?;
    project
        .present
        .iter()
        .enumerate()
        .filter(|(_, present)| present.scope == runner_core::Scope::Root)
        .filter_map(|(order, present)| {
            let pm = crate::provider::package_manager(REGISTRY.by_id(present.provider).label)?;
            writers.contains(&pm).then_some((order, present, pm))
        })
        .min_by_key(|(order, present, _)| {
            (
                !policy
                    .pm
                    .0
                    .values()
                    .any(|choice| choice.id == present.provider),
                *order,
            )
        })
        .map(|(_, _, pm)| pm)
        .ok_or(ResolveError::NoInstallers)
}

/// Print what the plan decided: the collisions the user asked for, then the
/// writers that lost a directory. A skipped install is never silent; that is
/// how a lockfile goes stale without anyone noticing.
fn report_plan(plan: &InstallPlan, overrides: &ResolutionOverrides) {
    if overrides.shows_warnings() {
        for collision in &plan.collisions {
            eprintln!(
                "{} install: {}",
                "warn:".yellow().bold(),
                collision_warning(collision.dir, &collision.writers),
            );
        }
    }
    if overrides.shows_progress() {
        for shadow in &plan.shadowed {
            eprintln!(
                "{}",
                format!(
                    "{}/: {} installs it, {} shadowed (enable both with `[tools.{}].install = \
                     true` and `[tools.{}].install = true`)",
                    shadow.dir,
                    shadow.winner.label(),
                    shadow.loser.label(),
                    shadow.winner.label(),
                    shadow.loser.label(),
                )
                .dimmed(),
            );
        }
    }
}

/// Run a single PM's install in the foreground, inheriting stdio.
fn install_single(
    execution: &InstallExecution,
    pm: ProviderId,
    overrides: &ResolutionOverrides,
) -> Result<i32> {
    if overrides.shows_progress() {
        eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
    }
    let plan = execution.plan(pm.label(), &[], overrides)?;
    let mut cmd = runner_core::execute::command(&plan)?;
    super::configure_task_streams(&mut cmd, overrides, "install");
    if overrides.explain {
        crate::render::explain::print_plan(overrides, &plan);
        crate::render::explain::print_command(overrides, &cmd);
        return Ok(0);
    }
    let mut child = runner_core::execute::spawn(&plan, &mut cmd)
        .map_err(|error| spawn_error(pm, cmd.get_program(), error))?;
    let status =
        wait_or_reap(&mut child).map_err(|error| wait_error(pm, cmd.get_program(), error))?;
    Ok(if status.success() {
        0
    } else {
        super::exit_code(status)
    })
}

/// Wait for the child; on a failed wait, stop and reap it so neither the
/// process nor its pipe writers outlive the error, then return that error.
fn wait_or_reap(child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
    match child.wait() {
        Ok(status) => Ok(status),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// Name the package manager and executable when an install cannot start.
fn spawn_error(pm: ProviderId, program: &OsStr, error: std::io::Error) -> anyhow::Error {
    let program = program.to_string_lossy();
    let context = if error.kind() == std::io::ErrorKind::NotFound {
        format!(
            "installing with {}: `{program}` was not found on PATH (pick the package manager with \
             --pm or `[pm]`)",
            pm.label(),
        )
    } else {
        format!(
            "installing with {}: `{program}` failed to launch",
            pm.label()
        )
    };
    anyhow::Error::new(error).context(context)
}

/// Name the package manager and executable when waiting on a started
/// install fails.
fn wait_error(pm: ProviderId, program: &OsStr, error: std::io::Error) -> anyhow::Error {
    anyhow::Error::new(error).context(format!(
        "installing with {}: waiting for `{}` failed",
        pm.label(),
        program.to_string_lossy()
    ))
}

/// Split the plan into lanes: the package managers that share an install
/// directory form one lane and run in sequence; everything else gets a lane of
/// its own. Lanes are emitted in detection order, as are the managers inside
/// one.
fn install_lanes(plan: &InstallPlan) -> Vec<Vec<ProviderId>> {
    let mut lanes: Vec<Vec<ProviderId>> = Vec::new();
    for pm in &plan.pms {
        if lanes.iter().flatten().any(|queued| queued == pm) {
            continue;
        }
        match plan
            .collisions
            .iter()
            .find(|collision| collision.writers.contains(pm))
        {
            Some(collision) => lanes.push(collision.writers.clone()),
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
/// non-zero exit code (by detection order); let the other lanes finish on their
/// own. A failure *inside* a lane stops that lane, because the next manager in
/// it would install over the tree the failed one left behind.
fn run_installs_parallel(
    execution: &InstallExecution,
    plan: &InstallPlan,
    overrides: &ResolutionOverrides,
) -> Result<i32> {
    if overrides.explain {
        for pm in &plan.pms {
            install_single(execution, *pm, overrides)?;
        }
        return Ok(0);
    }
    super::print_output_explain(overrides, "install");
    let lanes = install_lanes(plan);
    let names: Vec<&str> = plan.pms.iter().map(|pm| pm.label()).collect();
    let width = prefix_width(&names);
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    let sink: Arc<dyn LineSink> = Arc::new(StdioSink);

    let outcomes: Vec<Result<Option<(ProviderId, i32)>>> = std::thread::scope(|scope| {
        // Every lane is spawned before any is joined: joining as we go would
        // serialize the lanes, which is the whole thing this function exists
        // not to do.
        let mut handles = Vec::with_capacity(lanes.len());
        for lane in &lanes {
            let sink = Arc::clone(&sink);
            handles.push(
                scope.spawn(move || run_lane(execution, lane, overrides, &sink, width, colorize)),
            );
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

    let mut failures: Vec<(ProviderId, i32)> = Vec::new();
    for outcome in outcomes {
        match outcome {
            Ok(Some(failure)) => failures.push(failure),
            Ok(None) => {}
            Err(e) => return Err(e),
        }
    }
    let index_of = |pm: ProviderId| plan.pms.iter().position(|p| *p == pm);
    Ok(failures
        .into_iter()
        .min_by_key(|(pm, _)| index_of(*pm))
        .map_or(0, |(_, code)| code))
}

/// Run one lane's installs back to back, returning the lane's first failure.
fn run_lane(
    execution: &InstallExecution,
    lane: &[ProviderId],
    overrides: &ResolutionOverrides,
    sink: &Arc<dyn LineSink>,
    width: usize,
    colorize: bool,
) -> Result<Option<(ProviderId, i32)>> {
    for pm in lane {
        if overrides.shows_progress() {
            eprintln!("{} {}", "installing with".dimmed(), pm.label().bold());
        }
        let plan = execution.plan(pm.label(), &[], overrides)?;
        let mut cmd = runner_core::execute::command(&plan)?;
        let (stdout_policy, stderr_policy) = overrides.task_streams_for("install");
        cmd.stdin(Stdio::null())
            .stdout(match stdout_policy {
                tool::TaskStream::Inherit => Stdio::piped(),
                tool::TaskStream::Discard => Stdio::null(),
            })
            .stderr(match stderr_policy {
                tool::TaskStream::Inherit => Stdio::piped(),
                tool::TaskStream::Discard => Stdio::null(),
            });
        let mut child = runner_core::execute::spawn(&plan, &mut cmd)
            .map_err(|error| spawn_error(*pm, cmd.get_program(), error))?;
        let prefix = if overrides.emits_groups() {
            render_prefix(pm.label(), width, colorize)
        } else {
            String::new()
        };
        let mut streams: Vec<(String, bool, Box<dyn std::io::Read + Send>)> = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            streams.push((prefix.clone(), false, Box::new(stdout)));
        }
        if let Some(stderr) = child.stderr.take() {
            streams.push((prefix, true, Box::new(stderr)));
        }
        let delivery = Arc::new(Delivery::new(Arc::clone(sink)));
        let readers = spawn_readers(streams, &(delivery.clone() as Arc<dyn LineSink>));

        let waited = wait_or_reap(&mut child);
        for handle in readers {
            join_reader_thread(handle);
        }
        let status = waited.map_err(|error| wait_error(*pm, cmd.get_program(), error))?;
        if !status.success() {
            return Ok(Some((*pm, super::exit_code(status))));
        }
        if delivery.failed() {
            return Ok(Some((*pm, 1)));
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

/// Print one line per selected manager that cannot honour the script policy.
///
/// The line survives `--no-warnings`: a dropped deny leaves install-time code
/// running, and a dropped allow leaves the request unapplied.
fn disclose_script_clamps(
    execution: &InstallExecution,
    pms: &[ProviderId],
    overrides: &ResolutionOverrides,
) -> Result<()> {
    if !(overrides.shows_warnings()
        || overrides.no_warnings && overrides.quiet_level <= tool::QuietLevel::Quiet)
    {
        return Ok(());
    }
    for line in script_clamps(execution, pms, overrides)? {
        eprintln!("{} {line}", "warn:".yellow().bold());
    }
    Ok(())
}

/// The script-policy clamps of the selected managers' install plans.
fn script_clamps(
    execution: &InstallExecution,
    pms: &[ProviderId],
    overrides: &ResolutionOverrides,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for pm in pms {
        let plan = execution.plan(pm.label(), &[], overrides)?;
        for clamp in plan
            .clamps
            .iter()
            .filter(|clamp| clamp.requested.starts_with("scripts="))
        {
            lines.push(match overrides.script_policy {
                ScriptPolicy::Allow => format!(
                    "{} cannot force install scripts on; only the {} allowlist, which runner does \
                     not write, re-enables them",
                    pm.label(),
                    clamp.reason
                ),
                _ => format!(
                    "{} cannot skip install scripts; deny policy not applied to it",
                    pm.label()
                ),
            });
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use crate::provider::Named;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::InstallFlags;
    use runner_core::{ScriptMechanism, ScriptRequest};

    use super::{
        CollisionDir, InstallExecution, InstallPlan, Shadowed, install_lanes, install_task,
        plan_install, script_clamps, select_installers, spawn_error, tools_step,
    };
    use crate::resolver::{
        CollisionPolicy, FallbackPolicy, OverrideOrigin, PmOverride, ResolutionOverrides,
        ResolveError, ScriptPolicy,
    };
    use crate::types::{ProjectContext, Task, Workspace, WorkspaceMember};
    use runner_core::{Ecosystem, ProviderId};

    fn context(pms: &[ProviderId]) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        for pm in pms {
            crate::tool::test_support::write_signal(&root, *pm);
        }
        let mut ctx = ProjectContext {
            cwd: root.clone(),
            root,
            tasks: Vec::new(),
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        };
        crate::tool::test_support::seed_context(&mut ctx);
        ctx
    }

    fn override_pm(pm: ProviderId, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..Default::default()
        }
    }

    #[test]
    fn tools_step_runs_mise_when_detected() {
        let mut ctx = context(&[ProviderId::Npm]);
        crate::tool::test_support::declare(&mut ctx, ProviderId::Mise);
        assert_eq!(
            tools_step(
                &ctx,
                &ResolutionOverrides::default(),
                InstallFlags::default()
            ),
            Some(ProviderId::Mise)
        );
    }

    #[test]
    fn tools_step_is_absent_without_mise_config() {
        let mut ctx = context(&[ProviderId::Npm]);
        crate::tool::test_support::declare(&mut ctx, ProviderId::Just);
        assert_eq!(
            tools_step(
                &ctx,
                &ResolutionOverrides::default(),
                InstallFlags::default()
            ),
            None
        );
    }

    #[test]
    fn tools_step_honours_no_tools_flag() {
        let mut ctx = context(&[]);
        crate::tool::test_support::declare(&mut ctx, ProviderId::Mise);
        let flags = InstallFlags {
            no_tools: true,
            ..InstallFlags::default()
        };
        assert_eq!(
            tools_step(&ctx, &ResolutionOverrides::default(), flags),
            None
        );
    }

    fn install_task_in(member: Option<Arc<WorkspaceMember>>) -> Task {
        Task {
            name: "install".to_string(),
            source: ProviderId::Just,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member,
        }
    }

    fn workspace(members: Vec<Arc<WorkspaceMember>>, current: usize) -> Workspace {
        Workspace {
            root: PathBuf::from("/tmp/test"),
            kinds: vec!["pnpm-workspace.yaml"],
            current: Some(Arc::clone(&members[current])),
            members,
        }
    }

    #[test]
    fn install_task_prefers_the_current_member_over_the_root() {
        let app = Arc::new(WorkspaceMember::new(
            "app".to_string(),
            "apps/app".to_string(),
            PathBuf::from("/tmp/test/apps/app"),
        ));
        let mut ctx = context(&[]);
        ctx.workspace = Some(workspace(vec![Arc::clone(&app)], 0));
        ctx.tasks = vec![install_task_in(None), install_task_in(Some(app))];

        let task = install_task(&ctx).expect("an install task");
        assert!(task.member.is_some(), "the member's task wins");
        assert_eq!(ctx.spelling(task), "install");
    }

    #[test]
    fn install_task_ignores_other_members() {
        let app = Arc::new(WorkspaceMember::new(
            "app".to_string(),
            "apps/app".to_string(),
            PathBuf::from("/tmp/test/apps/app"),
        ));
        let lib = Arc::new(WorkspaceMember::new(
            "lib".to_string(),
            "libs/lib".to_string(),
            PathBuf::from("/tmp/test/libs/lib"),
        ));
        let mut ctx = context(&[]);
        ctx.workspace = Some(workspace(vec![Arc::clone(&app), Arc::clone(&lib)], 0));
        ctx.tasks = vec![install_task_in(Some(lib))];

        assert!(install_task(&ctx).is_none());
    }

    #[test]
    fn install_task_falls_back_to_the_root_from_a_member() {
        let app = Arc::new(WorkspaceMember::new(
            "app".to_string(),
            "apps/app".to_string(),
            PathBuf::from("/tmp/test/apps/app"),
        ));
        let mut ctx = context(&[]);
        ctx.workspace = Some(workspace(vec![app], 0));
        ctx.tasks = vec![install_task_in(None)];

        let task = install_task(&ctx).expect("the root install task");
        assert!(task.member.is_none());
    }

    #[test]
    fn missing_install_executable_is_named() {
        let mut command = std::process::Command::new("definitely-not-on-path-xyz");
        let error = command
            .spawn()
            .expect_err("a nonexistent program must not spawn");
        let message = format!(
            "{:#}",
            spawn_error(ProviderId::Uv, command.get_program(), error)
        );
        assert!(message.contains("installing with uv"), "{message}");
        assert!(
            message.contains("`definitely-not-on-path-xyz` was not found on PATH"),
            "{message}"
        );
        assert!(message.contains("--pm"), "{message}");
        assert!(message.contains("`[pm]`"), "{message}");
    }

    #[test]
    fn other_spawn_failures_name_the_program_without_the_path_hint() {
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let message = format!(
            "{:#}",
            spawn_error(ProviderId::Npm, std::ffi::OsStr::new("npm"), error)
        );
        assert!(message.contains("installing with npm"), "{message}");
        assert!(message.contains("`npm` failed to launch"), "{message}");
        assert!(message.contains("denied"), "{message}");
        assert!(!message.contains("--pm"), "{message}");
    }

    #[test]
    fn wait_failures_name_the_package_manager_and_program() {
        let error = std::io::Error::new(std::io::ErrorKind::Interrupted, "signal");
        let message = format!(
            "{:#}",
            super::wait_error(ProviderId::Pnpm, std::ffi::OsStr::new("pnpm"), error)
        );
        assert!(
            message.contains("installing with pnpm: waiting for `pnpm` failed"),
            "{message}"
        );
        assert!(message.contains("signal"), "{message}");
    }

    #[test]
    fn wait_or_reap_returns_the_status_of_a_finished_child() {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit 0"][..]
            } else {
                &[][..]
            })
            .spawn()
            .expect("spawns");
        let status = super::wait_or_reap(&mut child).expect("wait succeeds");
        assert!(status.success());
    }

    #[test]
    fn no_override_installs_with_every_detected_pm() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Deno]);
        let pms = select_installers(&ctx.package_managers(), &ResolutionOverrides::default())
            .expect("default selection should succeed");
        assert_eq!(pms, vec![ProviderId::Bun, ProviderId::Deno]);
    }

    #[test]
    fn detected_override_installs_with_it_alone() {
        // The dreamcli CI bug: bun + deno detected, RUNNER_PM=bun set,
        // deno must not install (and must not write deno.lock).
        let ctx = context(&[ProviderId::Bun, ProviderId::Deno]);
        let overrides = override_pm(ProviderId::Bun, OverrideOrigin::EnvVar);
        let pms = select_installers(&ctx.package_managers(), &overrides)
            .expect("detected override should filter");
        assert_eq!(pms, vec![ProviderId::Bun]);
    }

    #[test]
    fn an_unobserved_install_choice_is_refused() {
        let ctx = context(&[]);
        let overrides = override_pm(ProviderId::Npm, OverrideOrigin::CliFlag);
        let err = select_installers(&ctx.package_managers(), &overrides)
            .expect_err("no observed installer");
        assert!(format!("{err}").contains("npm"));
    }

    #[test]
    fn explicit_pm_preserves_per_tool_veto() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Pnpm]);
        let mut overrides = override_pm(ProviderId::Bun, OverrideOrigin::CliFlag);
        overrides.tool_install.insert("bun".into(), Vec::new());
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn undetected_override_errors_with_origin_and_detected_list() {
        let ctx = context(&[ProviderId::Cargo]);
        let overrides = override_pm(ProviderId::Npm, OverrideOrigin::EnvVar);
        let err = select_installers(&ctx.package_managers(), &overrides)
            .expect_err("undetected override must error");

        assert!(matches!(err, ResolveError::PmOverrideNotDetected { .. }));
        let msg = format!("{err}");
        assert!(msg.contains("RUNNER_PM"), "should name the source: {msg}");
        assert!(msg.contains("cargo"), "should list detected PMs: {msg}");
    }

    #[test]
    fn undetected_cli_override_names_the_flag() {
        let ctx = context(&[ProviderId::Cargo]);
        let overrides = override_pm(ProviderId::Npm, OverrideOrigin::CliFlag);
        let err = select_installers(&ctx.package_managers(), &overrides)
            .expect_err("undetected override must error");

        let msg = format!("{err}");
        assert!(msg.contains("--pm"), "should name the flag: {msg}");
    }

    #[test]
    fn per_tool_veto_filters_detected_installers() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Deno, ProviderId::Cargo]);
        let overrides = ResolutionOverrides {
            tool_install: [("deno".into(), Vec::new()), ("cargo".into(), Vec::new())].into(),
            ..Default::default()
        };
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides).unwrap(),
            [ProviderId::Bun]
        );
    }

    #[test]
    fn node_modules_writers_recorded_for_bun_plus_deno_node_modules_dir() {
        let dir = crate::tool::test_support::TempDir::new("detect-collision");
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        std::fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        std::fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ "nodeModulesDir": "auto" }"#,
        )
        .expect("deno.jsonc");

        let ctx = crate::detect::detect(dir.path(), &ResolutionOverrides::default());

        let writers = super::install_dirs(&ctx, &ctx.package_managers())
            .into_iter()
            .find(|entry| entry.dir == "node_modules")
            .map(|entry| entry.writers)
            .expect("bun + node_modules-dir deno both write node_modules");
        assert_eq!(writers, vec![ProviderId::Bun, ProviderId::Deno]);
    }

    #[test]
    fn deno_is_no_node_modules_writer_when_it_opts_out_of_a_local_tree() {
        let dir = crate::tool::test_support::TempDir::new("detect-no-collision");
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        std::fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        // Explicit `none` overrides the package.json default of `manual`, so
        // deno resolves npm packages from its global cache.
        std::fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ "nodeModulesDir": "none" }"#,
        )
        .expect("deno.jsonc");

        let ctx = crate::detect::detect(dir.path(), &ResolutionOverrides::default());

        let writers = super::install_dirs(&ctx, &ctx.package_managers())
            .into_iter()
            .find(|entry| entry.dir == "node_modules")
            .map(|entry| entry.writers)
            .expect("bun still writes node_modules");
        assert_eq!(writers, vec![ProviderId::Bun]);
    }

    #[test]
    fn a_deno_project_with_a_package_json_writes_node_modules_without_being_told_to() {
        // The shape that used to slip through: no `nodeModulesDir` line at all,
        // so runner said deno kept its deps in the global cache, while
        // `deno install` was in fact populating node_modules alongside bun.
        let dir = crate::tool::test_support::TempDir::new("detect-implicit-collision");
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        std::fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        std::fs::write(dir.path().join("deno.jsonc"), r#"{ "tasks": {} }"#).expect("deno.jsonc");

        let ctx = crate::detect::detect(dir.path(), &ResolutionOverrides::default());

        let writers = super::install_dirs(&ctx, &ctx.package_managers())
            .into_iter()
            .find(|entry| entry.dir == "node_modules")
            .map(|entry| entry.writers)
            .expect("both write node_modules");
        assert_eq!(writers, vec![ProviderId::Bun, ProviderId::Deno]);
    }

    /// bun + deno both writing `node_modules`, which is dreamcli's shape.
    fn colliding_context() -> ProjectContext {
        let mut ctx = context(&[ProviderId::Bun, ProviderId::Deno]);
        std::fs::write(ctx.root.join("package.json"), "{}").unwrap();
        crate::tool::test_support::seed_context(&mut ctx);
        ctx
    }

    #[test]
    fn colliding_writers_resolve_to_one_installer_by_default() {
        let ctx = colliding_context();
        let plan = plan_install(&ctx, &ResolutionOverrides::default()).expect("plan");

        assert_eq!(plan.pms, vec![ProviderId::Bun], "deno must not install");
        assert_eq!(
            plan.shadowed,
            vec![Shadowed {
                loser: ProviderId::Deno,
                winner: ProviderId::Bun,
                dir: "node_modules",
            }],
        );
        assert!(plan.collisions.is_empty(), "resolved, so nothing to warn");
    }

    #[test]
    fn ecosystem_pm_override_hands_the_tree_to_deno() {
        // `[pm].node = "deno"` picks deno for package.json scripts; the same
        // decision decides who owns node_modules, so bun is the one shadowed.
        let mut ctx = colliding_context();
        let mut overrides = ResolutionOverrides::default();
        overrides.pm_by_ecosystem.insert(
            Ecosystem::Deno,
            PmOverride {
                pm: ProviderId::Deno,
                origin: OverrideOrigin::ConfigFile {
                    path: PathBuf::from("/tmp/test/runner.toml"),
                },
            },
        );
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);

        let plan = plan_install(&ctx, &overrides).expect("plan");

        assert_eq!(plan.pms, vec![ProviderId::Deno]);
        assert_eq!(
            plan.shadowed,
            vec![Shadowed {
                loser: ProviderId::Bun,
                winner: ProviderId::Deno,
                dir: "node_modules",
            }],
        );
    }

    #[test]
    fn naming_both_writers_runs_both_serialized_with_one_warning() {
        let ctx = colliding_context();
        let overrides = ResolutionOverrides {
            tool_install: [
                ("bun".into(), vec!["install".into()]),
                ("deno".into(), vec!["install".into()]),
            ]
            .into(),
            ..Default::default()
        };

        let plan = plan_install(&ctx, &overrides).expect("plan");

        assert_eq!(plan.pms, vec![ProviderId::Bun, ProviderId::Deno]);
        assert!(plan.shadowed.is_empty(), "consent means nothing is dropped");
        assert_eq!(
            plan.collisions,
            vec![CollisionDir {
                dir: "node_modules",
                writers: vec![ProviderId::Bun, ProviderId::Deno],
            }],
        );
        assert_eq!(
            install_lanes(&plan),
            vec![vec![ProviderId::Bun, ProviderId::Deno]],
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
        assert!(msg.contains("install = false"), "msg: {msg}");
    }

    #[test]
    fn on_collision_error_refuses_even_when_both_were_named() {
        // The strict CI guard means "never two writers on one tree", so an
        // explicit allowlist doesn't buy its way past it.
        let ctx = colliding_context();
        let overrides = ResolutionOverrides {
            tool_install: [
                ("bun".into(), vec!["install".into()]),
                ("deno".into(), vec!["install".into()]),
            ]
            .into(),
            on_collision: CollisionPolicy::Error,
            ..Default::default()
        };

        let err = plan_install(&ctx, &overrides).expect_err("must refuse");
        assert!(matches!(err, ResolveError::InstallDirCollision { .. }));
    }

    #[test]
    fn pm_override_leaves_one_writer_and_nothing_to_say() {
        let ctx = colliding_context();
        let overrides = override_pm(ProviderId::Bun, OverrideOrigin::EnvVar);

        let plan = plan_install(&ctx, &overrides).expect("plan");

        assert_eq!(plan.pms, vec![ProviderId::Bun]);
        assert_eq!(plan.collisions.len(), 0);
        assert!(
            plan.shadowed.is_empty(),
            "nothing was dropped: deno was never in the install set to begin with"
        );
    }

    #[test]
    fn a_lone_writer_plans_clean() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Cargo]);

        let plan = plan_install(&ctx, &ResolutionOverrides::default()).expect("plan");

        assert_eq!(plan.pms, vec![ProviderId::Bun, ProviderId::Cargo]);
        assert_eq!(plan.collisions.len(), 0);
        assert_eq!(plan.shadowed.len(), 0);
    }

    #[test]
    fn lanes_serialize_shared_writers_and_leave_the_rest_parallel() {
        let plan = InstallPlan {
            pms: vec![ProviderId::Bun, ProviderId::Cargo, ProviderId::Deno],
            shadowed: Vec::new(),
            collisions: vec![CollisionDir {
                dir: "node_modules",
                writers: vec![ProviderId::Bun, ProviderId::Deno],
            }],
        };

        assert_eq!(
            install_lanes(&plan),
            vec![
                vec![ProviderId::Bun, ProviderId::Deno],
                vec![ProviderId::Cargo],
            ],
            "bun and deno share one lane and run in order; cargo runs alongside",
        );
    }

    #[test]
    fn per_tool_veto_preserves_detection_order() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Cargo, ProviderId::Uv]);
        let overrides = ResolutionOverrides {
            tool_install: [("cargo".into(), Vec::new())].into(),
            ..Default::default()
        };
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides).unwrap(),
            [ProviderId::Bun, ProviderId::Uv]
        );
    }

    #[test]
    fn enabling_an_unobserved_tool_does_not_fabricate_evidence() {
        let ctx = context(&[ProviderId::Bun]);
        let overrides = ResolutionOverrides {
            tool_install: [("pnpm".into(), vec!["install".into()])].into(),
            ..Default::default()
        };
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides).unwrap(),
            [ProviderId::Bun]
        );
    }

    #[test]
    fn a_probe_fallback_invents_no_installer_without_evidence() {
        let ctx = context(&[]);
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Probe,
            ..Default::default()
        };
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn install_vetoes_cannot_be_overridden_by_fallback_policy() {
        let ctx = context(&[ProviderId::Npm]);
        let overrides = ResolutionOverrides {
            tool_install: [("npm".into(), Vec::new())].into(),
            fallback: FallbackPolicy::Probe,
            ..Default::default()
        };
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn pm_override_selects_among_enabled_installers() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Deno]);
        let mut overrides = override_pm(ProviderId::Deno, OverrideOrigin::EnvVar);
        overrides
            .tool_install
            .insert("bun".into(), vec!["install".into()]);
        assert_eq!(
            select_installers(&ctx.package_managers(), &overrides).unwrap(),
            [ProviderId::Deno]
        );
    }

    #[test]
    fn empty_install_pms_installs_with_every_detected_pm() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Cargo]);
        let pms = select_installers(&ctx.package_managers(), &ResolutionOverrides::default())
            .expect("no allowlist installs all");
        assert_eq!(pms, vec![ProviderId::Bun, ProviderId::Cargo]);
    }

    #[test]
    fn ecosystem_config_override_governs_the_install_set() {
        let ctx = context(&[ProviderId::Bun, ProviderId::Npm, ProviderId::Cargo]);
        let choose = |pm| {
            let mut pm_by_ecosystem = HashMap::new();
            pm_by_ecosystem.insert(
                Ecosystem::Node,
                PmOverride {
                    pm,
                    origin: OverrideOrigin::ConfigFile {
                        path: PathBuf::from("/tmp/test/runner.toml"),
                    },
                },
            );
            ResolutionOverrides {
                pm_by_ecosystem,
                ..Default::default()
            }
        };

        let pms = select_installers(&ctx.package_managers(), &choose(ProviderId::Npm))
            .expect("the ecosystem's choice is present");
        assert_eq!(pms, vec![ProviderId::Npm, ProviderId::Cargo]);

        select_installers(&ctx.package_managers(), &choose(ProviderId::Pnpm))
            .expect_err("a choice nothing shows is refused, in install as in run");
    }

    fn script_support(pm: ProviderId) -> runner_core::ScriptSupport {
        runner_providers::REGISTRY
            .by_label(pm.label())
            .and_then(|provider| provider.caps.install)
            .map_or(runner_core::ScriptSupport::NONE, |install| install.scripts)
    }

    fn install_argv(pm: ProviderId, scripts: ScriptRequest) -> Vec<String> {
        let ctx = context(&[pm]);
        let overrides = ResolutionOverrides {
            script_policy: match scripts {
                ScriptRequest::Default => ScriptPolicy::Default,
                ScriptRequest::Deny => ScriptPolicy::Deny,
                ScriptRequest::Allow => ScriptPolicy::Allow,
            },
            ..ResolutionOverrides::default()
        };
        let execution = InstallExecution::new(&ctx, &overrides, false).unwrap();
        execution
            .plan(pm.label(), &[], &overrides)
            .unwrap()
            .argv
            .iter()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn deny_support_classifies_every_pm() {
        for pm in [
            ProviderId::Npm,
            ProviderId::Pnpm,
            ProviderId::Bun,
            ProviderId::Composer,
        ] {
            assert!(
                matches!(script_support(pm).deny, ScriptMechanism::Flag(_)),
                "{} via flag",
                pm.label()
            );
        }
        assert!(matches!(
            script_support(ProviderId::Yarn).deny,
            ScriptMechanism::FlagAndEnv(..)
        ));
        assert_eq!(
            script_support(ProviderId::Deno).deny,
            ScriptMechanism::Default
        );
        for pm in [
            ProviderId::Cargo,
            ProviderId::Uv,
            ProviderId::Poetry,
            ProviderId::Pipenv,
            ProviderId::Go,
            ProviderId::Bundler,
        ] {
            assert_eq!(
                script_support(pm).deny,
                ScriptMechanism::Unsupported,
                "{} unsupported",
                pm.label(),
            );
        }
    }

    #[test]
    fn force_support_classifies_every_pm() {
        for pm in [ProviderId::Npm, ProviderId::Deno] {
            assert!(
                matches!(script_support(pm).allow, ScriptMechanism::Flag(_)),
                "{} via flag",
                pm.label(),
            );
        }
        for pm in [ProviderId::Bun, ProviderId::Pnpm] {
            assert!(
                matches!(script_support(pm).allow, ScriptMechanism::Warn(_)),
                "{} not expressible",
                pm.label(),
            );
        }
        for pm in [
            ProviderId::Yarn,
            ProviderId::Composer,
            ProviderId::Cargo,
            ProviderId::Uv,
            ProviderId::Poetry,
            ProviderId::Pipenv,
            ProviderId::Go,
            ProviderId::Bundler,
        ] {
            assert!(
                matches!(script_support(pm).allow, ScriptMechanism::Default),
                "{} already runs",
                pm.label(),
            );
        }
    }

    #[test]
    fn deny_appends_skip_flag_for_flag_managers() {
        assert_eq!(
            install_argv(ProviderId::Npm, ScriptRequest::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(ProviderId::Pnpm, ScriptRequest::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(ProviderId::Bun, ScriptRequest::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(ProviderId::Composer, ScriptRequest::Deny),
            ["install", "--no-scripts"]
        );
    }

    #[test]
    fn deny_is_noop_for_default_deny_and_unsupported_managers() {
        // deno already denies by default, no flag added.
        assert_eq!(
            install_argv(ProviderId::Deno, ScriptRequest::Deny),
            ["install"]
        );
        // cargo has no toggle; the deny is reported elsewhere, command unchanged.
        assert_eq!(
            install_argv(ProviderId::Cargo, ScriptRequest::Deny),
            ["fetch"]
        );
    }

    #[test]
    fn force_on_appends_flag_for_flag_managers() {
        // npm negates ignore-scripts; deno allows all via bare --allow-scripts.
        assert_eq!(
            install_argv(ProviderId::Npm, ScriptRequest::Allow),
            ["install", "--no-ignore-scripts"]
        );
        assert_eq!(
            install_argv(ProviderId::Deno, ScriptRequest::Allow),
            ["install", "--allow-scripts"]
        );
    }

    #[test]
    fn force_on_is_noop_for_already_runs_and_unforceable_managers() {
        // composer/cargo run scripts by default; force-on changes nothing.
        assert_eq!(
            install_argv(ProviderId::Composer, ScriptRequest::Allow),
            ["install"]
        );
        assert_eq!(
            install_argv(ProviderId::Cargo, ScriptRequest::Allow),
            ["fetch"]
        );
        // bun/pnpm gate dependency builds behind a manifest allowlist, no flag;
        // the request is disclosed from the plan's clamp instead.
        assert_eq!(
            install_argv(ProviderId::Bun, ScriptRequest::Allow),
            ["install"]
        );
        assert_eq!(
            install_argv(ProviderId::Pnpm, ScriptRequest::Allow),
            ["install"]
        );
    }

    #[test]
    fn default_adds_no_skip_flag() {
        assert_eq!(
            install_argv(ProviderId::Npm, ScriptRequest::Default),
            ["install"]
        );
        assert_eq!(
            install_argv(ProviderId::Composer, ScriptRequest::Default),
            ["install"]
        );
    }

    #[test]
    fn script_disclosures_come_from_the_plans_clamps() {
        let ctx = context(&[
            ProviderId::Cargo,
            ProviderId::Npm,
            ProviderId::Pnpm,
            ProviderId::Bun,
        ]);
        let pms = ctx.package_managers();
        for (policy, expected) in [
            (ScriptPolicy::Default, vec![]),
            (ScriptPolicy::Deny, vec!["cargo cannot skip"]),
            (
                ScriptPolicy::Allow,
                vec!["bun cannot force", "pnpm cannot force"],
            ),
        ] {
            let overrides = ResolutionOverrides {
                script_policy: policy,
                no_warnings: true,
                ..ResolutionOverrides::default()
            };
            let execution = InstallExecution::new(&ctx, &overrides, false).unwrap();
            let lines = script_clamps(&execution, &pms, &overrides).unwrap();
            assert_eq!(lines.len(), expected.len(), "{policy:?}: {lines:?}");
            for (line, start) in lines.iter().zip(expected) {
                assert!(line.starts_with(start), "{line}");
            }
        }
    }

    #[test]
    fn the_install_fallback_takes_only_root_package_managers() {
        use runner_core::{Evidence, Present, Project, ProviderId, Scope, Weight};

        let present = |provider: ProviderId, scope: Scope| Present {
            provider,
            scope: scope.clone(),
            version: None,
            bin_dirs: Vec::new(),
            because: vec![Evidence {
                provider: Some(provider),
                signal: None,
                at: PathBuf::from("/p/package.json"),
                scope,
                weight: Weight::Declared,
                declared: None,
            }],
        };
        let member = Scope::Member {
            name: "web".into(),
            dir: PathBuf::from("/p/web"),
        };
        let project = Project {
            present: vec![
                present(ProviderId::Npm, Scope::Root),
                present(ProviderId::Pnpm, member),
            ],
            ..Project::default()
        };
        assert_eq!(super::root_installers(&project), [ProviderId::Npm]);
    }
    #[test]
    fn a_probed_installer_needs_a_task_source_it_dispatches() {
        use runner_core::{Evidence, Present, Project, ProviderId, Scope, Weight};

        let present = |provider: ProviderId, weight: Weight| Present {
            provider,
            scope: Scope::Root,
            version: None,
            bin_dirs: Vec::new(),
            because: vec![Evidence {
                provider: Some(provider),
                signal: None,
                at: PathBuf::from("/p"),
                scope: Scope::Root,
                weight,
                declared: None,
            }],
        };
        let mixed = Project {
            present: vec![
                present(ProviderId::Cargo, Weight::Configured),
                present(ProviderId::PackageJson, Weight::Declared),
                present(ProviderId::Npm, Weight::Probed),
            ],
            ..Project::default()
        };
        assert_eq!(
            super::root_installers(&mixed),
            [ProviderId::Cargo, ProviderId::Npm]
        );
        let chosen_only = Project {
            present: vec![
                present(ProviderId::Cargo, Weight::Configured),
                present(ProviderId::Volta, Weight::Present),
                present(ProviderId::Npm, Weight::Probed),
            ],
            ..Project::default()
        };
        assert_eq!(super::root_installers(&chosen_only), [ProviderId::Cargo]);
    }
}
