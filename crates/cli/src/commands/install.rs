//! `runner install`, install dependencies via every detected package manager.

use std::any::Any;
use std::ffi::OsStr;
use std::process::Stdio;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Result, bail};
use colored::Colorize;
use runner_core::{Provider, ScriptMechanism, ScriptSupport};
use runner_providers::REGISTRY;

use crate::chain::mux::{LineSink, StdioSink, prefix_width, render_prefix, spawn_readers};
use crate::resolver::{CollisionPolicy, ResolutionOverrides, ResolveError, Resolver, ScriptPolicy};

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
    let tools = tools_step(ctx, flags);
    let task = install_task(ctx);
    // Planned before the GHA group opens so a refused override doesn't
    // emit an empty `runner: install` group.
    let plan = if ctx.package_managers.is_empty() {
        match plan_from_resolver(ctx, overrides, sink.as_deref_mut()) {
            Ok(plan) => plan,
            Err(err) if (tools.is_some() || task.is_some()) && is_no_signals(&err) => {
                if !overrides.install_pms.is_empty() {
                    return Err(ResolveError::InstallPmsNotDetected {
                        missing: overrides.install_pms.clone(),
                        detected: Vec::new(),
                    }
                    .into());
                }
                InstallPlan::empty()
            }
            Err(err) => return Err(err),
        }
    } else {
        plan_install(ctx, overrides)?
    };

    let mut execution = InstallExecution::new(ctx, overrides, flags.frozen, &plan.pms)?;
    report_plan(&plan, overrides);
    warn_unsupported_script_policy(&plan.pms, overrides, &execution.project);
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
pub(crate) fn tools_step(ctx: &ProjectContext, flags: InstallFlags) -> Option<TaskRunner> {
    (!flags.no_tools && ctx.task_runners.contains(&TaskRunner::Mise)).then_some(TaskRunner::Mise)
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
        Some(ResolveError::NoSignalsFound { .. })
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
    fn new(
        ctx: &ProjectContext,
        overrides: &ResolutionOverrides,
        frozen: bool,
        managers: &[PackageManager],
    ) -> Result<Self> {
        let mut policy = super::run::core::policy(overrides);
        policy.frozen = frozen;
        policy.scripts = match overrides.script_policy {
            ScriptPolicy::Default => runner_core::ScriptPolicy::Default,
            ScriptPolicy::Deny => runner_core::ScriptPolicy::Deny,
            ScriptPolicy::Allow => runner_core::ScriptPolicy::Allow,
        };
        for pm in managers {
            let provider = provider(pm.label());
            policy
                .pm
                .0
                .entry(provider.ecosystem)
                .or_insert(runner_core::Choice {
                    id: provider.id,
                    from: runner_core::Layer::Probe,
                });
        }
        let requested = overrides.host_verbosity_for("install");
        policy.verbosity = match requested.diagnostics {
            tool::HostDiagnostics::Normal => runner_core::Verbosity::Normal,
            tool::HostDiagnostics::Quiet => runner_core::Verbosity::Quiet,
            tool::HostDiagnostics::Reduced => runner_core::Verbosity::VeryQuiet,
        };
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
            .present
            .iter()
            .find(|p| p.provider == provider.id)
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
    let mut cmd = runner_core::execute::command(&plan);
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
        // The tools this just installed were invisible to `mise bin-paths`
        // a moment ago, and the package managers spawn next.
        super::forget_mise_bin_dirs();
        return Ok(None);
    }
    Ok(Some(super::exit_code(status)))
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

/// Install plan for a project without a single detected PM signal: the
/// same resolver chain `runner run` dispatches through (PM override,
/// manifest field, PATH probe, fallback policy) picks one PM, so a bare
/// `package.json` installs with npm instead of erroring. The resolver
/// refuses when no `package.json` exists upward, so non-Node directories
/// still fail with its no-signals error.
fn plan_from_resolver(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    sink: super::WarningSink<'_>,
) -> Result<InstallPlan> {
    let decision = Resolver::new(ctx, overrides)
        .resolve_node_pm()
        .map_err(anyhow::Error::from)?;
    if overrides.pm.is_none() {
        let missing: Vec<PackageManager> = overrides
            .install_pms
            .iter()
            .copied()
            .filter(|pm| *pm != decision.pm)
            .collect();
        if !missing.is_empty() {
            return Err(ResolveError::InstallPmsNotDetected {
                missing,
                detected: vec![decision.pm],
            }
            .into());
        }
    }
    super::print_warning_slice(&decision.warnings, overrides, sink);
    Ok(InstallPlan {
        pms: vec![decision.pm],
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
        // Naming several writers in the allowlist is consent: the user knows
        // both write the tree and wants both to run. Runner honours it, warns
        // once, and serializes them so consent doesn't become corruption.
        if consented_to(&writers, overrides) {
            plan.collisions.push(CollisionDir {
                dir: install_dir.dir,
                writers,
            });
            continue;
        }
        let winner = dir_winner(ctx, overrides, &writers);
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
    let first = writers.first().map_or("bun", |pm| pm.label());
    format!(
        "{list} all install into {dir}/, and the install allowlist names them all, so they run \
         one after another over the same tree instead of one of them being skipped. Drop all but \
         `{first}` from `[install].pms` (or `RUNNER_INSTALL_PMS`) to skip the redundant install."
    )
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
/// The only shared directory today is `node_modules`, whose writers are all
/// node-ecosystem, so the winner is the PM the resolver already picks for
/// `package.json` scripts (lockfile, `packageManager`, `[pm].node`, PATH
/// probe). Reusing that decision keeps one project from having two different
/// primary package managers depending on whether it runs a script or an
/// install, and lets `[pm].node = "deno"` hand deno the tree without a second
/// knob.
///
/// The winner logic is therefore node-specific. A future non-node shared
/// directory would not match the node resolver's pick and would fall back to
/// the first writer in detection order, a deterministic default that whoever
/// adds that directory should replace with real resolution for its ecosystem.
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
                    "{}/: {} installs it, {} shadowed (run both with `[install].pms = [\"{}\", \
                     \"{}\"]`)",
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
    let mut cmd = runner_core::execute::command(&plan);
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
             --pm or `[install].pms`)",
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
#[allow(
    clippy::too_many_arguments,
    reason = "one lane's worth of the parallel executor's state; bundling it into a struct buys \
              nothing at one call site"
)]
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
        let mut cmd = runner_core::execute::command(&plan);
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

/// Whether the resolved [`ScriptPolicy`] asks to skip install scripts.
const fn deny_scripts(overrides: &ResolutionOverrides) -> bool {
    matches!(overrides.script_policy, ScriptPolicy::Deny)
}

/// Whether the resolved [`ScriptPolicy`] asks to force install scripts on.
const fn force_scripts(overrides: &ResolutionOverrides) -> bool {
    matches!(overrides.script_policy, ScriptPolicy::Allow)
}

/// The script mechanisms `pm` declares for its install.
fn script_support(pm: PackageManager, project: &runner_core::Project) -> ScriptSupport {
    REGISTRY
        .effective(provider(pm.label()).id, project, &runner_core::Scope::Root)
        .caps
        .install
        .map_or(ScriptSupport::NONE, |install| install.scripts)
}

/// The manifest allowlist that re-enables scripts for a manager whose
/// force-on is not flag-expressible.
fn force_allowlist(pm: PackageManager, project: &runner_core::Project) -> Option<&'static str> {
    match script_support(pm, project).allow {
        ScriptMechanism::Warn(allowlist) => Some(allowlist),
        ScriptMechanism::Flag(_)
        | ScriptMechanism::Env(..)
        | ScriptMechanism::Default
        | ScriptMechanism::Unsupported => None,
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
/// - **deny** is a security-relevant disclosure: the unsupported managers
///   execute arbitrary install-time code and have no flag to skip it, so a
///   dropped deny must never hide.
/// - **force-on** is a request-fidelity disclosure: a manager that denies
///   dependency build scripts by default and re-enables them only through a
///   manifest allowlist runner won't write cannot apply `--scripts`.
fn warn_unsupported_script_policy(
    pms: &[PackageManager],
    overrides: &ResolutionOverrides,
    project: &runner_core::Project,
) {
    if !(overrides.shows_warnings()
        || overrides.no_warnings && overrides.quiet_level <= tool::QuietLevel::Quiet)
    {
        return;
    }
    for pm in unsupported_deny_managers(pms, overrides, project) {
        eprintln!(
            "{} {} cannot skip install scripts; deny policy not applied to it",
            "warn:".yellow().bold(),
            pm.label(),
        );
    }
    for pm in unforceable_managers(pms, overrides, project) {
        eprintln!(
            "{} {} cannot force install scripts on; it denies dependency build scripts by default \
             and only the {} allowlist (which runner won't write) re-enables them",
            "warn:".yellow().bold(),
            pm.label(),
            force_allowlist(pm, project).unwrap_or("manifest"),
        );
    }
}

/// Selected managers that cannot honor an active deny-scripts policy, in
/// selection order. Empty unless the policy is [`ScriptPolicy::Deny`].
fn unsupported_deny_managers(
    pms: &[PackageManager],
    overrides: &ResolutionOverrides,
    project: &runner_core::Project,
) -> Vec<PackageManager> {
    if !deny_scripts(overrides) {
        return Vec::new();
    }
    pms.iter()
        .copied()
        .filter(|pm| {
            matches!(
                script_support(*pm, project).deny,
                ScriptMechanism::Unsupported
            )
        })
        .collect()
}

/// Selected managers whose force-scripts-on request cannot be expressed by a
/// flag, in selection order. Empty unless the policy is [`ScriptPolicy::Allow`].
fn unforceable_managers(
    pms: &[PackageManager],
    overrides: &ResolutionOverrides,
    project: &runner_core::Project,
) -> Vec<PackageManager> {
    if !force_scripts(overrides) {
        return Vec::new();
    }
    pms.iter()
        .copied()
        .filter(|pm| force_allowlist(*pm, project).is_some())
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
    use std::sync::Arc;

    use super::InstallFlags;
    use runner_core::{ScriptMechanism, ScriptRequest};

    use super::{
        CollisionDir, InstallExecution, InstallPlan, Shadowed, install_lanes, install_task,
        is_no_signals, plan_install, script_support, select_install_pms, spawn_error, tools_step,
        unforceable_managers, unsupported_deny_managers, warn_unsupported_script_policy,
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
            tools_step(&ctx, InstallFlags::default()),
            Some(TaskRunner::Mise)
        );
    }

    #[test]
    fn tools_step_is_absent_without_mise_config() {
        let mut ctx = context(vec![PackageManager::Npm]);
        ctx.task_runners.push(TaskRunner::Just);
        assert_eq!(tools_step(&ctx, InstallFlags::default()), None);
    }

    #[test]
    fn tools_step_honours_no_tools_flag() {
        let mut ctx = context(vec![]);
        ctx.task_runners.push(TaskRunner::Mise);
        let flags = InstallFlags {
            no_tools: true,
            ..InstallFlags::default()
        };
        assert_eq!(tools_step(&ctx, flags), None);
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
        let other: anyhow::Error = ResolveError::InstallPmsNotDetected {
            missing: vec![PackageManager::Bun],
            detected: vec![],
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
        assert!(message.contains("`[install].pms`"), "{message}");
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
    #[ignore = "docs/architecture.md section 10 step 5: one resolver"]
    fn a_forced_pm_without_any_evidence_is_refused() {
        let ctx = context(Vec::new());
        let overrides = override_pm(PackageManager::Npm, OverrideOrigin::CliFlag);
        let err = super::plan_from_resolver(&ctx, &overrides, None)
            .expect_err("a plan needs evidence; a PATH probe would be the least of it");
        assert!(format!("{err:#}").contains("npm"), "{err:#}");
    }

    #[test]
    #[ignore = "docs/architecture.md section 10 step 5: one resolver"]
    fn pm_override_outranks_install_allowlist_only_over_evidence() {
        let ctx = context(vec![PackageManager::Bun, PackageManager::Pnpm]);
        let overrides = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            install_pms: vec![PackageManager::Pnpm],
            ..Default::default()
        };
        let plan = super::plan_from_resolver(&ctx, &overrides, None)
            .expect("--pm outranks the install allowlist over a detected manager");
        assert_eq!(plan.pms, vec![PackageManager::Bun]);

        let bare = context(Vec::new());
        super::plan_from_resolver(&bare, &overrides, None)
            .expect_err("no evidence means no plan, whatever the layer order says");
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
            install_pms: vec![PackageManager::Bun, PackageManager::Deno],
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
    #[ignore = "docs/architecture.md section 10 step 5: one resolver"]
    fn an_allowlist_with_no_evidence_is_refused_whatever_the_fallback_says() {
        let ctx = context(Vec::new());
        for install_pms in [
            vec![PackageManager::Npm, PackageManager::Pnpm],
            vec![PackageManager::Npm],
        ] {
            let overrides = ResolutionOverrides {
                install_pms,
                fallback: FallbackPolicy::Npm,
                ..Default::default()
            };
            let err = super::plan_from_resolver(&ctx, &overrides, None)
                .expect_err("the override chain has no npm fallback layer");
            let err = err
                .downcast_ref::<ResolveError>()
                .expect("resolver error survives the anyhow wrapper");
            assert!(
                matches!(err, ResolveError::NoSignalsFound { .. }),
                "nothing was observed, so nothing is present: {err:?}",
            );
        }
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
    #[ignore = "docs/architecture.md section 10 step 5: one resolver"]
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

    fn install_argv(pm: PackageManager, scripts: ScriptRequest) -> Vec<String> {
        let ctx = context(vec![pm]);
        let overrides = ResolutionOverrides {
            script_policy: match scripts {
                ScriptRequest::Default => ScriptPolicy::Default,
                ScriptRequest::Deny => ScriptPolicy::Deny,
                ScriptRequest::Allow => ScriptPolicy::Allow,
            },
            ..ResolutionOverrides::default()
        };
        let execution = InstallExecution::new(&ctx, &overrides, false, &[pm]).unwrap();
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
            PackageManager::Yarn,
            PackageManager::Pnpm,
            PackageManager::Bun,
            PackageManager::Composer,
        ] {
            assert!(
                matches!(
                    script_support(pm, &runner_core::Project::default()).deny,
                    ScriptMechanism::Flag(_)
                ),
                "{} via flag",
                pm.label()
            );
        }
        assert_eq!(
            script_support(PackageManager::Deno, &runner_core::Project::default()).deny,
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
                script_support(pm, &runner_core::Project::default()).deny,
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
                matches!(
                    script_support(pm, &runner_core::Project::default()).allow,
                    ScriptMechanism::Flag(_)
                ),
                "{} via flag",
                pm.label(),
            );
        }
        for pm in [PackageManager::Bun, PackageManager::Pnpm] {
            assert!(
                matches!(
                    script_support(pm, &runner_core::Project::default()).allow,
                    ScriptMechanism::Warn(_)
                ),
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
                matches!(
                    script_support(pm, &runner_core::Project::default()).allow,
                    ScriptMechanism::Default | ScriptMechanism::Unsupported
                ),
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
        // the request is disclosed by warn_unsupported_script_policy instead.
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
    fn deny_disclosure_fires_for_unsupported_pms_regardless_of_no_warnings() {
        let pms = [PackageManager::Cargo, PackageManager::Npm];
        // Non-deny policy: nothing to disclose.
        assert!(
            unsupported_deny_managers(
                &pms,
                &ResolutionOverrides::default(),
                &runner_core::Project::default()
            )
            .is_empty()
        );
        // Deny + --no-warnings: the unsupported PM (cargo) is still disclosed,
        // because this is a security notice, not a cosmetic warning. Npm honors
        // the deny via a flag, so it is not listed.
        let denying = ResolutionOverrides {
            script_policy: ScriptPolicy::Deny,
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            unsupported_deny_managers(&pms, &denying, &runner_core::Project::default()),
            vec![PackageManager::Cargo]
        );
        // Smoke the public entry point with the same inputs: still emits, no panic.
        warn_unsupported_script_policy(&pms, &denying, &runner_core::Project::default());
    }

    #[test]
    fn force_disclosure_fires_for_unforceable_pms_regardless_of_no_warnings() {
        let pms = [
            PackageManager::Pnpm,
            PackageManager::Npm,
            PackageManager::Bun,
        ];
        // Non-force policy: nothing to disclose.
        assert!(
            unforceable_managers(
                &pms,
                &ResolutionOverrides::default(),
                &runner_core::Project::default()
            )
            .is_empty()
        );
        // Force-on + --no-warnings: pnpm and bun (manifest-allowlist managers)
        // are still disclosed, in selection order. npm expresses force-on via a
        // flag, so it is not listed.
        let forcing = ResolutionOverrides {
            script_policy: ScriptPolicy::Allow,
            no_warnings: true,
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            unforceable_managers(&pms, &forcing, &runner_core::Project::default()),
            vec![PackageManager::Pnpm, PackageManager::Bun]
        );
        // Smoke the public entry point with the same inputs: still emits, no panic.
        warn_unsupported_script_policy(&pms, &forcing, &runner_core::Project::default());
    }
}
