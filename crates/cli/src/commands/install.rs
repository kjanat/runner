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

use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};
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
use crate::types::{PackageManager, ProjectContext, TaskRunner, version_matches};

/// Install dependencies for each detected package manager.
///
/// Warns when the current Node.js version doesn't match the project's
/// expected version before proceeding. Thin wrapper over [`install_pms`]
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
    let plan = if ctx.package_managers.is_empty() {
        match plan_from_resolver(ctx, overrides, sink.as_deref_mut()) {
            Ok(plan) => plan,
            Err(err) if (declared_tools.is_some() || task.is_some()) && is_no_signals(&err) => {
                InstallPlan::empty()
            }
            Err(err) => return Err(err),
        }
    } else {
        plan_install(ctx, overrides)?
    };

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

    if overrides.shows_warnings()
        && let (Some(nv), Some(cur)) = (&ctx.node_version, &ctx.current_node)
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
) -> Option<TaskRunner> {
    ctx.task_runners.iter().copied().find(|runner| {
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

/// `true` when the resolver found nothing to install with. A project that
/// only declares tools (a `mise.toml` without a manifest) still has a
/// meaningful `runner install`: the toolchain step alone.
/// The project's own `install` task, run when no package manager has
/// anything to install: the current workspace member's, else the root's.
/// `runner why install` already names it.
fn install_task(ctx: &ProjectContext) -> Option<&crate::types::Task> {
    ctx.tasks
        .iter()
        .filter(|task| task.name == "install" && ctx.is_local(task))
        .min_by_key(|task| ctx.scope_rank(task))
}

fn is_no_signals(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<ResolveError>(),
        Some(ResolveError::NoSignalsFound { .. } | ResolveError::NoInstallers)
    )
}

/// Run the toolchain step in the foreground. Returns the tool manager's
/// exit code when it fails, so the package managers that may depend on the
/// tools it installs never run against a half-installed toolchain. A
/// missing binary is a warning: the config describes tools the host may
/// already have on `PATH`.
fn run_tools_step(
    execution: &InstallExecution,
    runner: TaskRunner,
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
    runner: TaskRunner,
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
        let project = super::run::core::project_under(ctx, &policy)?;
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
    runner: TaskRunner,
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
fn select_install_pms(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
) -> Result<Vec<PackageManager>, ResolveError> {
    select_installers(&ctx.package_managers, overrides)
}

fn select_installers(
    detected: &[PackageManager],
    overrides: &ResolutionOverrides,
) -> Result<Vec<PackageManager>, ResolveError> {
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

/// Resolve installers from observed provider capabilities.
fn plan_from_resolver(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    sink: super::WarningSink<'_>,
) -> Result<InstallPlan> {
    let policy = super::run::core::policy(overrides);
    let project = super::run::core::project_under(ctx, &policy)?;
    super::print_core_warnings(&project.warnings, overrides, sink);
    let mut observed: Vec<_> = project
        .present
        .iter()
        .filter(|present| {
            REGISTRY
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
        .filter_map(|present| PackageManager::from_label(REGISTRY.by_id(present.provider).label))
        .collect();
    observed.dedup();
    if observed.is_empty() && overrides.pm.is_none() && overrides.pm_by_ecosystem.is_empty() {
        return Err(ResolveError::NoInstallers.into());
    }
    Ok(InstallPlan {
        pms: select_installers(&observed, overrides)?,
        shadowed: Vec::new(),
        collisions: Vec::new(),
    })
}

/// The same selection as [`select_install_pms`] with the not-detected
/// errors dropped: an override naming an absent PM narrows to nothing
/// instead of failing. Reporting surfaces need the effective set without
/// inheriting dispatch's fatal cases; `doctor` must survive the broken
/// config it exists to diagnose.
fn effective_install_pms(
    detected: &[PackageManager],
    overrides: &ResolutionOverrides,
) -> Vec<PackageManager> {
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
    pub writers: Vec<PackageManager>,
}

/// A writer dropped from the install set because another manager owns the
/// directory it would have written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shadowed {
    pub loser: PackageManager,
    pub winner: PackageManager,
    pub dir: &'static str,
}

/// What this invocation will install with, and what it decided about the
/// directories two package managers would otherwise write at once.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InstallPlan {
    /// The package managers that actually run, in detection order.
    pub pms: Vec<PackageManager>,
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

/// The warning shown when the install set keeps two or more writers on one
/// directory. They run serially, so nothing corrupts, but the redundant second
/// install is worth flagging.
pub(crate) fn collision_warning(dir: &str, writers: &[PackageManager]) -> String {
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
fn consented_to(writers: &[PackageManager], overrides: &ResolutionOverrides) -> bool {
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
    writers: &[PackageManager],
) -> Result<PackageManager, ResolveError> {
    let policy = super::run::core::policy(overrides);
    let project =
        super::run::core::project_under(ctx, &policy).map_err(ResolveError::Observation)?;
    project
        .present
        .iter()
        .filter_map(|present| {
            let pm = PackageManager::from_label(REGISTRY.by_id(present.provider).label)?;
            writers.contains(&pm).then_some((present, pm))
        })
        .min_by_key(|(present, _)| {
            (
                !policy
                    .pm
                    .0
                    .values()
                    .any(|choice| choice.id == present.provider),
                present.because.iter().map(|evidence| evidence.weight).min(),
                present.provider,
            )
        })
        .map(|(_, pm)| pm)
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
    pm: PackageManager,
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
fn spawn_error(pm: PackageManager, program: &OsStr, error: std::io::Error) -> anyhow::Error {
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
fn wait_error(pm: PackageManager, program: &OsStr, error: std::io::Error) -> anyhow::Error {
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
fn install_lanes(plan: &InstallPlan) -> Vec<Vec<PackageManager>> {
    let mut lanes: Vec<Vec<PackageManager>> = Vec::new();
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

    let outcomes: Vec<Result<Option<(PackageManager, i32)>>> = std::thread::scope(|scope| {
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
fn run_lane(
    execution: &InstallExecution,
    lane: &[PackageManager],
    overrides: &ResolutionOverrides,
    sink: &Arc<dyn LineSink>,
    width: usize,
    colorize: bool,
) -> Result<Option<(PackageManager, i32)>> {
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
        let readers = spawn_readers(streams, sink);

        let waited = wait_or_reap(&mut child);
        for handle in readers {
            join_reader_thread(handle);
        }
        let status = waited.map_err(|error| wait_error(*pm, cmd.get_program(), error))?;
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

/// Print one line per selected manager that cannot honour the script policy.
///
/// The line survives `--no-warnings`: a dropped deny leaves install-time code
/// running, and a dropped allow leaves the request unapplied.
fn disclose_script_clamps(
    execution: &InstallExecution,
    pms: &[PackageManager],
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
    pms: &[PackageManager],
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
    use std::sync::Arc;

    use super::InstallFlags;
    use runner_core::{ScriptMechanism, ScriptRequest};

    use super::{
        CollisionDir, InstallExecution, InstallPlan, Shadowed, install_lanes, install_task,
        is_no_signals, plan_install, script_clamps, select_install_pms, spawn_error, tools_step,
    };
    use crate::resolver::{
        CollisionPolicy, FallbackPolicy, OverrideOrigin, PmOverride, ResolutionOverrides,
        ResolveError, ScriptPolicy,
    };
    use crate::types::{
        Ecosystem, InstallDir, PackageManager, ProjectContext, Task, TaskRunner, TaskSource,
        Workspace, WorkspaceKind, WorkspaceMember,
    };

    fn context(pms: Vec<PackageManager>) -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("/tmp/test"),
            root: PathBuf::from("/tmp/test"),
            package_managers: pms,
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
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
    fn tools_step_runs_mise_when_detected() {
        let mut ctx = context(vec![PackageManager::Npm]);
        ctx.task_runners.push(TaskRunner::Mise);
        assert_eq!(
            tools_step(
                &ctx,
                &ResolutionOverrides::default(),
                InstallFlags::default()
            ),
            Some(TaskRunner::Mise)
        );
    }

    #[test]
    fn tools_step_is_absent_without_mise_config() {
        let mut ctx = context(vec![PackageManager::Npm]);
        ctx.task_runners.push(TaskRunner::Just);
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
        let mut ctx = context(vec![]);
        ctx.task_runners.push(TaskRunner::Mise);
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
            source: TaskSource::Justfile,
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
            kinds: vec![WorkspaceKind::PnpmWorkspace],
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
        let mut ctx = context(vec![]);
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
        let mut ctx = context(vec![]);
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
        let mut ctx = context(vec![]);
        ctx.workspace = Some(workspace(vec![app], 0));
        ctx.tasks = vec![install_task_in(None)];

        let task = install_task(&ctx).expect("the root install task");
        assert!(task.member.is_none());
    }

    #[test]
    fn no_signals_error_is_recognised_through_anyhow() {
        let err: anyhow::Error = ResolveError::NoSignalsFound {
            ecosystem: Ecosystem::Node,
            soft: true,
        }
        .into();
        assert!(is_no_signals(&err));
        let other: anyhow::Error = ResolveError::InvalidOverride {
            value: "npm".into(),
            reason: "test refusal",
        }
        .into();
        assert!(!is_no_signals(&other));
    }

    #[test]
    fn missing_install_executable_is_named() {
        let mut command = std::process::Command::new("definitely-not-on-path-xyz");
        let error = command
            .spawn()
            .expect_err("a nonexistent program must not spawn");
        let message = format!(
            "{:#}",
            spawn_error(PackageManager::Uv, command.get_program(), error)
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
            spawn_error(PackageManager::Npm, std::ffi::OsStr::new("npm"), error)
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
            super::wait_error(PackageManager::Pnpm, std::ffi::OsStr::new("pnpm"), error)
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
    fn an_unobserved_install_choice_is_refused() {
        let ctx = context(Vec::new());
        let overrides = override_pm(PackageManager::Npm, OverrideOrigin::CliFlag);
        let err = select_install_pms(&ctx, &overrides).expect_err("no observed installer");
        assert!(format!("{err}").contains("npm"));
    }

    #[test]
    fn explicit_pm_preserves_per_tool_veto() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Pnpm]);
        let mut overrides = override_pm(PackageManager::Bun, OverrideOrigin::CliFlag);
        overrides.tool_install.insert("bun".into(), Vec::new());
        assert!(select_install_pms(&ctx, &overrides).unwrap().is_empty());
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
    fn per_tool_veto_filters_detected_installers() {
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Deno,
            PackageManager::Cargo,
        ]);
        let overrides = ResolutionOverrides {
            tool_install: [("deno".into(), Vec::new()), ("cargo".into(), Vec::new())].into(),
            ..Default::default()
        };
        assert_eq!(
            select_install_pms(&ctx, &overrides).unwrap(),
            [PackageManager::Bun]
        );
    }

    /// bun + deno both writing `node_modules`, which is dreamcli's shape.
    fn colliding_context() -> ProjectContext {
        let mut ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        ctx.root = crate::tool::test_support::project_root();
        ctx.cwd = ctx.root.clone();
        crate::tool::test_support::seed_context(&ctx);
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
            vec![Shadowed {
                loser: PackageManager::Deno,
                winner: PackageManager::Bun,
                dir: "node_modules",
            }],
        );
        assert!(plan.collisions.is_empty(), "resolved, so nothing to warn");
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
            vec![Shadowed {
                loser: PackageManager::Bun,
                winner: PackageManager::Deno,
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

        assert_eq!(plan.pms, vec![PackageManager::Bun, PackageManager::Deno]);
        assert!(plan.shadowed.is_empty(), "consent means nothing is dropped");
        assert_eq!(
            plan.collisions,
            vec![CollisionDir {
                dir: "node_modules",
                writers: vec![PackageManager::Bun, PackageManager::Deno],
            }],
        );
        assert_eq!(
            install_lanes(&plan),
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
            collisions: vec![CollisionDir {
                dir: "node_modules",
                writers: vec![PackageManager::Bun, PackageManager::Deno],
            }],
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
    fn per_tool_veto_preserves_detection_order() {
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Cargo,
            PackageManager::Uv,
        ]);
        let overrides = ResolutionOverrides {
            tool_install: [("cargo".into(), Vec::new())].into(),
            ..Default::default()
        };
        assert_eq!(
            select_install_pms(&ctx, &overrides).unwrap(),
            [PackageManager::Bun, PackageManager::Uv]
        );
    }

    #[test]
    fn enabling_an_unobserved_tool_does_not_fabricate_evidence() {
        let ctx = context(vec![PackageManager::Bun]);
        let overrides = ResolutionOverrides {
            tool_install: [("pnpm".into(), vec!["install".into()])].into(),
            ..Default::default()
        };
        assert_eq!(
            select_install_pms(&ctx, &overrides).unwrap(),
            [PackageManager::Bun]
        );
    }

    #[test]
    fn an_npm_fallback_invents_no_installer_without_evidence() {
        let ctx = context(Vec::new());
        let overrides = ResolutionOverrides {
            fallback: FallbackPolicy::Npm,
            ..Default::default()
        };
        assert!(select_install_pms(&ctx, &overrides).unwrap().is_empty());
    }

    #[test]
    fn install_vetoes_cannot_be_overridden_by_fallback_policy() {
        let ctx = context(vec![PackageManager::Npm]);
        let overrides = ResolutionOverrides {
            tool_install: [("npm".into(), Vec::new())].into(),
            fallback: FallbackPolicy::Npm,
            ..Default::default()
        };
        assert!(select_install_pms(&ctx, &overrides).unwrap().is_empty());
    }

    #[test]
    fn pm_override_selects_among_enabled_installers() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Deno]);
        let mut overrides = override_pm(PackageManager::Deno, OverrideOrigin::EnvVar);
        overrides
            .tool_install
            .insert("bun".into(), vec!["install".into()]);
        assert_eq!(
            select_install_pms(&ctx, &overrides).unwrap(),
            [PackageManager::Deno]
        );
    }

    #[test]
    fn empty_install_pms_installs_with_every_detected_pm() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Cargo]);
        let pms = select_install_pms(&ctx, &ResolutionOverrides::default())
            .expect("no allowlist installs all");
        assert_eq!(pms, vec![PackageManager::Bun, PackageManager::Cargo]);
    }

    #[test]
    fn ecosystem_config_override_governs_the_install_set() {
        let ctx = context(vec![
            PackageManager::Bun,
            PackageManager::Npm,
            PackageManager::Cargo,
        ]);
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

        let pms = select_install_pms(&ctx, &choose(PackageManager::Npm))
            .expect("the ecosystem's choice is present");
        assert_eq!(pms, vec![PackageManager::Npm, PackageManager::Cargo]);

        select_install_pms(&ctx, &choose(PackageManager::Pnpm))
            .expect_err("a choice nothing shows is refused, in install as in run");
    }

    fn script_support(pm: PackageManager) -> runner_core::ScriptSupport {
        runner_providers::REGISTRY
            .by_label(pm.label())
            .and_then(|provider| provider.caps.install)
            .map_or(runner_core::ScriptSupport::NONE, |install| install.scripts)
    }

    fn install_argv(pm: PackageManager, scripts: ScriptRequest) -> Vec<String> {
        let mut ctx = context(vec![pm]);
        ctx.root = crate::tool::test_support::project_root();
        ctx.cwd = ctx.root.clone();
        crate::tool::test_support::seed_context(&ctx);
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
            PackageManager::Npm,
            PackageManager::Pnpm,
            PackageManager::Bun,
            PackageManager::Composer,
        ] {
            assert!(
                matches!(script_support(pm).deny, ScriptMechanism::Flag(_)),
                "{} via flag",
                pm.label()
            );
        }
        assert!(matches!(
            script_support(PackageManager::Yarn).deny,
            ScriptMechanism::FlagAndEnv(..)
        ));
        assert_eq!(
            script_support(PackageManager::Deno).deny,
            ScriptMechanism::Default
        );
        for pm in [
            PackageManager::Cargo,
            PackageManager::Uv,
            PackageManager::Poetry,
            PackageManager::Pipenv,
            PackageManager::Go,
            PackageManager::Bundler,
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
        for pm in [PackageManager::Npm, PackageManager::Deno] {
            assert!(
                matches!(script_support(pm).allow, ScriptMechanism::Flag(_)),
                "{} via flag",
                pm.label(),
            );
        }
        for pm in [PackageManager::Bun, PackageManager::Pnpm] {
            assert!(
                matches!(script_support(pm).allow, ScriptMechanism::Warn(_)),
                "{} not expressible",
                pm.label(),
            );
        }
        for pm in [
            PackageManager::Yarn,
            PackageManager::Composer,
            PackageManager::Cargo,
            PackageManager::Uv,
            PackageManager::Poetry,
            PackageManager::Pipenv,
            PackageManager::Go,
            PackageManager::Bundler,
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
            install_argv(PackageManager::Npm, ScriptRequest::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Pnpm, ScriptRequest::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Bun, ScriptRequest::Deny),
            ["install", "--ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Composer, ScriptRequest::Deny),
            ["install", "--no-scripts"]
        );
    }

    #[test]
    fn deny_is_noop_for_default_deny_and_unsupported_managers() {
        // deno already denies by default, no flag added.
        assert_eq!(
            install_argv(PackageManager::Deno, ScriptRequest::Deny),
            ["install"]
        );
        // cargo has no toggle; the deny is reported elsewhere, command unchanged.
        assert_eq!(
            install_argv(PackageManager::Cargo, ScriptRequest::Deny),
            ["fetch"]
        );
    }

    #[test]
    fn force_on_appends_flag_for_flag_managers() {
        // npm negates ignore-scripts; deno allows all via bare --allow-scripts.
        assert_eq!(
            install_argv(PackageManager::Npm, ScriptRequest::Allow),
            ["install", "--no-ignore-scripts"]
        );
        assert_eq!(
            install_argv(PackageManager::Deno, ScriptRequest::Allow),
            ["install", "--allow-scripts"]
        );
    }

    #[test]
    fn force_on_is_noop_for_already_runs_and_unforceable_managers() {
        // composer/cargo run scripts by default; force-on changes nothing.
        assert_eq!(
            install_argv(PackageManager::Composer, ScriptRequest::Allow),
            ["install"]
        );
        assert_eq!(
            install_argv(PackageManager::Cargo, ScriptRequest::Allow),
            ["fetch"]
        );
        // bun/pnpm gate dependency builds behind a manifest allowlist, no flag;
        // the request is disclosed from the plan's clamp instead.
        assert_eq!(
            install_argv(PackageManager::Bun, ScriptRequest::Allow),
            ["install"]
        );
        assert_eq!(
            install_argv(PackageManager::Pnpm, ScriptRequest::Allow),
            ["install"]
        );
    }

    #[test]
    fn default_adds_no_skip_flag() {
        assert_eq!(
            install_argv(PackageManager::Npm, ScriptRequest::Default),
            ["install"]
        );
        assert_eq!(
            install_argv(PackageManager::Composer, ScriptRequest::Default),
            ["install"]
        );
    }

    #[test]
    fn script_disclosures_come_from_the_plans_clamps() {
        let mut ctx = context(vec![
            PackageManager::Cargo,
            PackageManager::Npm,
            PackageManager::Pnpm,
            PackageManager::Bun,
        ]);
        ctx.root = crate::tool::test_support::project_root();
        ctx.cwd = ctx.root.clone();
        crate::tool::test_support::seed_context(&ctx);
        let pms = ctx.package_managers.clone();
        for (policy, expected) in [
            (ScriptPolicy::Default, vec![]),
            (ScriptPolicy::Deny, vec!["cargo cannot skip"]),
            (
                ScriptPolicy::Allow,
                vec!["pnpm cannot force", "bun cannot force"],
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
}
