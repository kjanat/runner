//! Plan run requests through the core, add invocation metadata and stdio,
//! then execute the plan through the guarded executor. Provider declarations
//! own argv construction; the CLI retains presentation and launch diagnostics.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Child, Command, ExitStatus};

use anyhow::{Result, anyhow, bail};

use super::decision::PmDecision;
use super::runtime;
use crate::render::arrow::print_dispatch_arrow;
use crate::resolver::ResolutionOverrides;
use crate::tool;
use crate::types::{JsRuntime, ProjectContext, Task, TaskSource};

fn print_pm_explain(overrides: &ResolutionOverrides, describe: &str) {
    crate::commands::print_explain(overrides, &format!("resolved: {describe}"));
}

/// `--explain` line for the workspace scope a task was picked from: which
/// scope won, why it outranked the others, and the directory it runs in.
fn print_scope_explain(ctx: &ProjectContext, overrides: &ResolutionOverrides, entry: &Task) {
    let Some(workspace) = ctx.workspace.as_ref() else {
        return;
    };
    let candidates: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|task| task.name == entry.name)
        .collect();
    let scope = match (&entry.member, workspace.current.as_ref()) {
        (Some(member), Some(current)) if member.dir == current.dir => {
            format!("{} (current member)", member.name)
        }
        (Some(member), _) => format!("{} (member)", member.name),
        (None, _) => "root".to_string(),
    };
    let mut others: Vec<&str> = candidates
        .iter()
        .filter(|candidate| !candidate.same_scope(entry))
        .map(|candidate| candidate.scope())
        .collect();
    others.sort_unstable();
    others.dedup();
    let outranked = if others.is_empty() {
        String::new()
    } else {
        format!(", outranks {}", others.join(", "))
    };
    crate::commands::print_explain(
        overrides,
        &format!(
            "scope: {scope}{outranked}; cwd={} root={} dir={}",
            ctx.cwd.display(),
            ctx.root.display(),
            entry.dir(&ctx.root).display(),
        ),
    );
}

/// Refuse a make target, or a package script that is a bare `make <name>`
/// wrapper, given anything but variable assignments, before the arrow,
/// since make would take the word as an option or a goal.
fn check_make_args(entry: &Task, args: &[String]) -> Result<()> {
    let wraps_make = entry.passthrough_to == Some(crate::types::TaskRunner::Make);
    if entry.source != TaskSource::Makefile && !wraps_make {
        return Ok(());
    }
    let Some(word) = tool::make::first_non_assignment(args) else {
        return Ok(());
    };
    let subject = if wraps_make {
        format!(
            "{} script {:?}, which runs `make {}`,",
            entry.source.label(),
            entry.name,
            entry.name
        )
    } else {
        format!("make target {:?}", entry.name)
    };
    bail!(
        "{subject} cannot take {word:?}: GNU make has no recipe-argument passthrough, so it would \
         parse the word as its own option or as another goal. Pass `NAME=value` assignments the \
         Makefile reads as `$(NAME)`, or invoke the command itself by path (`run \
         ./node_modules/.bin/<command> <args>`), which outranks task lookup"
    )
}

/// Refuse a mise task whose spec marks a flag required when that flag is
/// absent. No-op for every other source, none of which declares a spec.
///
/// Runs before the dispatch arrow: without it the failure lands after mise
/// has started the task and whatever it builds has run, and in a parallel
/// chain the siblings are already going.
fn check_mise_usage(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    entry: &Task,
    args: &[String],
    sink: crate::commands::WarningSink<'_>,
) -> Result<()> {
    if entry.source != TaskSource::MiseToml {
        return Ok(());
    }
    let task = entry.name.as_str();
    let spec = match tool::mise::usage_spec(entry.dir(&ctx.root), task) {
        Ok(Some(spec)) => spec,
        Ok(None) => return Ok(()),
        Err(error) => {
            crate::commands::print_warning_slice(
                &[crate::types::DetectionWarning::Pipeline(
                    runner_core::Warning::about(
                        runner_core::ProviderId::Mise,
                        format!("{error:#}; required flags were not checked"),
                    ),
                )],
                overrides,
                sink,
            );
            return Ok(());
        }
    };
    let missing = spec.missing_required_flags(args);
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "{task} requires {}\n  usage: {task} {}",
        missing.join(", "),
        spec.signature,
    )
}

/// Outcome of resolving a task: a spawnable process, or a deno task to
/// run in-process via the embedded task shell.
#[derive(Debug)]
pub(super) enum Dispatch {
    /// A runner verb selected by the first cascade rung.
    Builtin(String),
    /// A configured process to spawn (`.status()` / `.spawn()`).
    Spawn(Box<SpawnDispatch>),
}

/// A command plus any resolver decision needed to diagnose spawn failure.
#[derive(Debug)]
pub(super) struct SpawnDispatch {
    pub(super) task_key: String,
    command: Command,
    plan: Box<runner_core::Plan>,
    diagnostic: SpawnDiagnostic,
}

#[derive(Debug)]
enum SpawnDiagnostic {
    Passthrough,
    PackageManager(PmDecision),
}

impl SpawnDispatch {
    #[cfg(test)]
    fn passthrough(command: Command) -> Self {
        let cwd = std::env::temp_dir();
        let tree = runner_core::Tree {
            root: cwd.clone(),
            cwd,
            members: vec![],
        };
        let argv = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(OsStr::to_os_string)
            .collect();
        let plan = runner_core::plan_argv(
            &tree,
            &runner_core::Project::default(),
            &runner_core::Policy::default(),
            command.get_program().into(),
            &runner_providers::REGISTRY,
            argv,
        )
        .expect("test plan");
        Self {
            task_key: String::new(),
            command,
            plan: Box::new(plan),
            diagnostic: SpawnDiagnostic::Passthrough,
        }
    }

    #[cfg(test)]
    fn package_manager(command: Command, decision: PmDecision) -> Self {
        Self {
            diagnostic: SpawnDiagnostic::PackageManager(decision),
            ..Self::passthrough(command)
        }
    }

    pub(super) const fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    pub(super) fn status(&mut self) -> Result<ExitStatus> {
        let result = runner_core::execute::status(&self.plan, &mut self.command);
        result.map_err(|error| self.spawn_error(error))
    }

    pub(super) fn spawn(&mut self) -> Result<Child> {
        let result = runner_core::execute::spawn(&self.plan, &mut self.command);
        result.map_err(|error| self.spawn_error(error))
    }

    fn spawn_error(&self, error: io::Error) -> anyhow::Error {
        let path = self.effective_env("PATH");
        let pathext = self.effective_env("PATHEXT");
        let selected_pm_present = match &self.diagnostic {
            SpawnDiagnostic::PackageManager(decision) => path
                .as_deref()
                .and_then(|path| {
                    crate::resolver::probe_path_for_doctor(
                        decision.pm.label(),
                        path,
                        pathext.as_deref(),
                    )
                })
                .is_some(),
            SpawnDiagnostic::Passthrough => false,
        };
        self.spawn_error_with_presence(error, selected_pm_present)
    }

    /// Resolve an environment variable exactly as this command will see it:
    /// an explicit command override/removal wins, otherwise inherit it.
    fn effective_env(&self, expected: &str) -> Option<OsString> {
        match self
            .command
            .get_envs()
            .find(|(key, _)| env_key_matches(key, expected))
        {
            Some((_, value)) => value.map(OsStr::to_owned),
            None => std::env::var_os(expected),
        }
    }

    fn spawn_error_with_presence(
        &self,
        error: io::Error,
        selected_pm_present: bool,
    ) -> anyhow::Error {
        match (&self.diagnostic, error.kind()) {
            (SpawnDiagnostic::PackageManager(decision), io::ErrorKind::NotFound)
                if !selected_pm_present =>
            {
                anyhow::Error::new(error).context(format!(
                    "{} was selected, but its executable was not found on PATH",
                    decision.describe(),
                ))
            }
            (SpawnDiagnostic::PackageManager(decision), io::ErrorKind::NotFound) => {
                anyhow::Error::new(error).context(format!(
                    "{} was selected, but failed to launch",
                    decision.describe()
                ))
            }
            _ => error.into(),
        }
    }
}

fn env_key_matches(actual: &OsStr, expected: &str) -> bool {
    #[cfg(windows)]
    {
        actual.to_string_lossy().eq_ignore_ascii_case(expected)
    }
    #[cfg(not(windows))]
    {
        actual == OsStr::new(expected)
    }
}

/// Resolve a token through the core cascade, or an explicit package operation.
/// No command is spawned here; both synchronous and parallel callers receive
/// the same complete plan, configured command and launch diagnostics.
pub(super) fn resolve_dispatch(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
    mut sink: crate::commands::WarningSink<'_>,
    _allow_self_exec: bool,
) -> Result<Dispatch> {
    crate::commands::print_warnings(ctx, overrides, sink.as_deref_mut());
    if let Some(selected) = dispatch_by_package(ctx, overrides, task, args, sink.as_deref_mut())? {
        return Ok(selected);
    }

    super::qualify::precheck_task(ctx, overrides, task)?;
    dispatch_plan(ctx, overrides, task, args, sink)
}

/// Print the arrow for a local file or installed binary and configure its
/// process for the project.
fn spawn_plan(
    overrides: &ResolutionOverrides,
    token: &str,
    args: &[String],
    mut plan: runner_core::Plan,
) -> Result<Dispatch> {
    crate::commands::configure_plan(&mut plan, overrides, token);
    crate::render::explain::print_plan(overrides, &plan);
    print_dispatch_arrow(overrides, token, &plan_label(&plan, token), token, args);
    let mut command = runner_core::execute::command(&plan)?;
    crate::commands::configure_task_streams(&mut command, overrides, token);
    let spawn = SpawnDispatch {
        task_key: token.to_owned(),
        command,
        plan: Box::new(plan),
        diagnostic: SpawnDiagnostic::Passthrough,
    };
    Ok(Dispatch::Spawn(Box::new(spawn)))
}

/// `--package <package> <bin>`: the binary from the package's own manifest
/// when it is installed, else the package manager's package-selecting exec.
/// Nothing else is consulted, so a same-named `.bin` link, a task or a file
/// cannot stand in for the package the user named. An `npm:` token is
/// refused with the equivalent form. `None` when no package was selected.
fn dispatch_by_package(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    bin: &str,
    args: &[String],
    sink: crate::commands::WarningSink<'_>,
) -> Result<Option<Dispatch>> {
    if let Some(spec) = bin.strip_prefix("npm:") {
        bail!(
            "`npm:{spec}` is not a task: name the package with `--package` and the binary as the \
             task, e.g. `run --package {spec} <bin>`"
        );
    }
    let Some(package) = overrides.package.as_deref() else {
        return Ok(None);
    };
    if let Some(dep) = super::local_dep::try_selected_package(ctx, overrides, package, bin, args)? {
        print_pm_explain(overrides, &dep.describe);
        return Ok(Some(spawn_plan(overrides, bin, args, dep.plan)?));
    }
    if let Some(shadow) = project_bin(&ctx.cwd, bin) {
        bail!(
            "{package} is not installed, and `{bin}` at {} belongs to another package; a fetched \
             {package} lacking `{bin}` would fall through to that one, so install {package} to \
             run its `{bin}`",
            shadow.display()
        );
    }

    let mut prepared = super::core::prepare(ctx, overrides, bin)?;
    let resolved_pm = match overrides.pm.as_ref() {
        Some(o) if !o.pm.can_dispatch_node_scripts() => {
            print_pm_explain(
                overrides,
                &format!("{} {}", o.pm.label(), o.origin.describe_pm_source()),
            );
            Some(o.pm)
        }
        _ => prepared
            .decision(runner_core::ProviderId::PackageJson)
            .map(|decision| {
                crate::commands::print_warning_slice(
                    &decision.warnings(&prepared.project, overrides),
                    overrides,
                    sink,
                );
                print_pm_explain(overrides, &decision.describe());
                decision.pm
            }),
    };
    if !runtime::replaces_exec(resolved_pm) {
        prepared.policy.runtime = None;
    }
    let chosen = prepared
        .policy
        .runtime
        .as_ref()
        .map(|choice| choice.id)
        .or_else(|| resolved_pm.and_then(|pm| super::core::provider(pm.label())));
    let present = chosen
        .and_then(|id| prepared.project.present.iter().find(|p| p.provider == id))
        .ok_or_else(|| {
            anyhow!(
                "{package} is not installed and no package manager was detected to fetch it; pick \
                 one with --pm"
            )
        })?;
    let plan = runner_core::plan_with(
        &prepared.tree,
        &prepared.project,
        &prepared.policy,
        present,
        &runner_core::Op::ExecPackage { package, bin, args },
        &runner_providers::REGISTRY,
    )
    .map_err(|refusal| match refusal {
        runner_core::Refusal::NoCapability { .. } => anyhow!(
            "{package} is not installed and {} has no package-selecting exec; install it or pick \
             another package manager with --pm",
            runner_providers::REGISTRY.by_id(present.provider).label
        ),
        other => refusal_error(ctx, bin, &other),
    })?;
    crate::commands::authorize_fetch(overrides, &format!("{package} ({bin})"), "exec-package")?;
    Ok(Some(spawn_plan(overrides, bin, args, plan)?))
}

/// Walk the complete core cascade and configure the selected plan for execution.
fn dispatch_plan(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task_name: &str,
    args: &[String],
    mut sink: crate::commands::WarningSink<'_>,
) -> Result<Dispatch> {
    let prepared = super::core::prepare(ctx, overrides, task_name)?;
    let decision = prepared.decision(runner_core::ProviderId::PackageJson);
    let resolved_pm = decision.as_ref().map(|decision| decision.pm);
    let requested = prepared.requested;
    let policy = &prepared.policy;
    let project = &prepared.project;
    crate::commands::print_core_warnings(&project.warnings, overrides, sink.as_deref_mut());
    if let Some(decision) = &decision {
        crate::commands::print_warning_slice(
            &decision.warnings(project, overrides),
            overrides,
            sink.as_deref_mut(),
        );
        print_pm_explain(overrides, &decision.describe());
    }
    if let Some(rt) = runtime::overridden(overrides)
        && !runtime::replaces_exec(resolved_pm)
    {
        runtime::report_unapplied_exec(overrides, rt, resolved_pm, sink.as_deref_mut());
    }

    let dep = |name: &str| {
        super::local_dep::installed_binary(ctx, name).map_err(|error| {
            match error.downcast::<io::Error>() {
                Ok(error) => error.into(),
                Err(error) => runner_core::Refusal::Invalid(error.to_string()),
            }
        })
    };
    let confirm = |name: &str, rung: &str| crate::commands::confirm_fetch(name, rung);
    let cascade = prepared.cascade(&dep, Some(&confirm));
    let (rung, dispatched) = runner_core::dispatch(&cascade, task_name, args)
        .map_err(|refusal| refusal_error(ctx, task_name, &refusal))?;
    let mut plan = match dispatched {
        runner_core::Dispatch::Builtin(name) => return Ok(Dispatch::Builtin(name)),
        runner_core::Dispatch::Plan(plan) => plan,
    };
    let entry = if rung.name == "task" {
        prepared.selected(ctx, task_name)?
    } else {
        None
    };
    let task_key = entry.map_or_else(|| task_name.to_owned(), super::task_output_key);
    complete_plan(
        ctx,
        overrides,
        &Chosen {
            token: task_name,
            args,
            rung,
            entry,
            key: &task_key,
        },
        &mut plan,
        sink.as_deref_mut(),
    )?;
    crate::commands::print_core_warnings(&plan.warnings, overrides, sink);
    explain_host(overrides, &plan, project, requested, policy.verbosity);
    crate::render::explain::print_plan(overrides, &plan);
    let label = entry.map_or_else(
        || plan_label(&plan, task_name),
        |entry| entry.source.label().to_string(),
    );
    if rung.name == "dep"
        && let Some(found) = &plan.found
    {
        let bin = found.file_name().unwrap_or_default().to_string_lossy();
        print_pm_explain(
            overrides,
            &format!("{bin} from {} (local dependency)", found.display()),
        );
    }
    let arrow_name = if rung.name == "test" {
        "test"
    } else {
        task_name
    };
    print_dispatch_arrow(overrides, task_name, &label, arrow_name, args);
    let mut cmd = runner_core::execute::command(&plan)?;
    let (stdout, stderr) = overrides.task_streams_for(&task_key);
    crate::commands::print_output_explain(overrides, &task_key);
    crate::commands::set_task_stdio(&mut cmd, stdout, stderr);
    let diagnostic = spawn_diagnostic(entry, overrides, &plan)?;
    let spawn = SpawnDispatch {
        task_key,
        command: cmd,
        plan,
        diagnostic,
    };
    Ok(Dispatch::Spawn(Box::new(spawn)))
}

/// The launch diagnostic for a task a package manager dispatches.
fn spawn_diagnostic(
    entry: Option<&Task>,
    overrides: &ResolutionOverrides,
    plan: &runner_core::Plan,
) -> Result<SpawnDiagnostic> {
    let managed = match entry.map(|e| e.source) {
        Some(TaskSource::PackageJson) => overrides.runtime.is_none(),
        Some(TaskSource::PyprojectScripts) => true,
        _ => false,
    };
    if !managed {
        return Ok(SpawnDiagnostic::Passthrough);
    }
    PmDecision::from_plan(plan)
        .map(SpawnDiagnostic::PackageManager)
        .ok_or_else(|| anyhow!("planned task has no package-manager choice"))
}

/// What the cascade chose for a token, and the `[tasks.<key>]` it answers to.
pub(super) struct Chosen<'a> {
    pub(super) token: &'a str,
    pub(super) args: &'a [String],
    pub(super) rung: runner_core::Rung,
    pub(super) entry: Option<&'a Task>,
    pub(super) key: &'a str,
}

pub(super) fn complete_plan(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    chosen: &Chosen<'_>,
    plan: &mut runner_core::Plan,
    sink: crate::commands::WarningSink<'_>,
) -> Result<()> {
    let entry = chosen.entry;
    prepare_task(ctx, overrides, entry, chosen.args, plan, sink)?;
    prepare_host(ctx, chosen.token, chosen.rung, plan)?;
    crate::commands::configure_plan(plan, overrides, chosen.key);
    if entry.is_some_and(|entry| entry.source == TaskSource::GoPackage) {
        preserve_go_environment(plan, &ctx.root)?;
    }
    runner_core::execute::command(plan)?;
    Ok(())
}

fn prepare_host(
    ctx: &ProjectContext,
    task_name: &str,
    rung: runner_core::Rung,
    plan: &mut runner_core::Plan,
) -> Result<()> {
    if rung.name == "host"
        && let Some(provider) = plan.provider
        && runner_providers::REGISTRY
            .by_id(provider)
            .caps
            .run_default
            .is_some()
        && let Some(source) =
            TaskSource::from_label(runner_providers::REGISTRY.by_id(provider).label)
    {
        let stack = crate::commands::push_task_frame(&ctx.root, source, task_name)?;
        plan.env
            .push((crate::commands::TASK_STACK_ENV.into(), stack));
    }
    Ok(())
}

fn prepare_task(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    entry: Option<&Task>,
    args: &[String],
    plan: &mut runner_core::Plan,
    mut sink: crate::commands::WarningSink<'_>,
) -> Result<()> {
    if let Some(entry) = entry {
        print_scope_explain(ctx, overrides, entry);
        check_mise_usage(ctx, overrides, entry, args, sink.as_deref_mut())?;
        runtime::report_unhonored(overrides, entry, sink.as_deref_mut());
        if entry.source == TaskSource::PackageJson
            && let Some(over) = &overrides.runtime
        {
            print_pm_explain(overrides, &over.describe());
            if over.runtime == JsRuntime::Node {
                runtime::warn_skipped_lifecycle(ctx, overrides, &entry.name, sink);
            }
        }

        check_make_args(entry, args)?;
        let stack =
            crate::commands::push_task_frame(entry.dir(&ctx.root), entry.source, &entry.name)?;
        plan.env
            .push((crate::commands::TASK_STACK_ENV.into(), stack));
    }
    Ok(())
}

/// Include the Go toolchain's VCS stamping environment in the plan.
fn preserve_go_environment(plan: &mut runner_core::Plan, root: &std::path::Path) -> Result<()> {
    let mut command = runner_core::execute::command(plan)?;
    tool::go_pm::stamp_vcs(&mut command, root);
    if let Some((key, Some(value))) = command.get_envs().find(|(key, _)| *key == "GOFLAGS") {
        plan.env.retain(|(name, _)| name != key);
        plan.env_remove.retain(|name| name != key);
        plan.env.push((key.to_owned(), value.to_owned()));
    }
    Ok(())
}

fn explain_host(
    overrides: &ResolutionOverrides,
    plan: &runner_core::Plan,
    project: &runner_core::Project,
    requested: tool::HostVerbosity,
    verbosity: runner_core::Verbosity,
) {
    if let Some(provider) = plan.provider {
        let descriptor = runner_providers::REGISTRY.by_id(provider);
        let quiet = project
            .present
            .iter()
            .find(|p| p.provider == provider)
            .map_or(descriptor.caps.quiet, |present| {
                descriptor.for_present(present).caps.quiet
            });
        let applied = requested.diagnostics.min(if quiet.strongest() == 0 {
            tool::HostDiagnostics::Normal
        } else {
            tool::HostDiagnostics::Reduced
        });
        let quiet_args = quiet
            .at(verbosity.index())
            .map(|template| template.render(&runner_core::Request::default()).args)
            .unwrap_or_default();
        crate::commands::print_explain(
            overrides,
            &format!(
                "host: {} diagnostics={} applied={} args=[{}] stream={} matrix={} limitation={:?}",
                descriptor.label,
                requested.diagnostics.label(),
                applied.label(),
                quiet_args
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
                requested.stream.label(),
                descriptor.label,
                quiet.limitation
            ),
        );
    }
}

/// The arrow label for a plan: the program plus the literal words it puts
/// before the name, which is how every exec primitive reads out loud.
fn plan_label(plan: &runner_core::Plan, name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut argv = plan.argv.iter().map(|word| word.to_string_lossy());
    let Some(program) = argv.next() else {
        return String::from("exec");
    };
    words.push(
        std::path::Path::new(program.as_ref())
            .file_stem()
            .map_or_else(
                || program.to_string(),
                |stem| stem.to_string_lossy().into_owned(),
            ),
    );
    if plan.provider.is_none() {
        return String::from("exec");
    }
    for word in argv {
        if word == name || word.starts_with('-') {
            break;
        }
        words.push(word.into_owned());
    }
    words.join(" ")
}

/// Turn a core refusal into the diagnostic the user reads.
fn refusal_error(
    ctx: &ProjectContext,
    task_name: &str,
    refusal: &runner_core::Refusal,
) -> anyhow::Error {
    use runner_core::Refusal;
    match refusal {
        Refusal::Invalid(message) => anyhow!("{message}"),
        Refusal::Observation { kind, message } => io::Error::new(*kind, message.clone()).into(),
        Refusal::UnsupportedFile {
            provider,
            file,
            reason,
            chosen_by,
            alternatives,
        } => {
            let origin = chosen_by
                .as_ref()
                .map_or_else(String::new, |layer| format!(" ({})", runtime_origin(layer)));
            let suggestions = runtime_suggestions(alternatives, &runner_providers::REGISTRY);
            let hint = if suggestions.is_empty() {
                String::new()
            } else {
                format!(" Try {}.", suggestions.join(" or "))
            };
            anyhow!(
                "{} cannot run {}{origin}: {reason}.{hint}",
                runner_providers::REGISTRY.by_id(*provider).label,
                file.display()
            )
        }
        Refusal::NotFound { name, tried } => {
            let rungs: Vec<&str> = tried.iter().map(|rung| rung.name).collect();
            anyhow!(
                "task {name:?} not found; tried {}. Run `runner list` to see available tasks.",
                rungs.join(", "),
            )
        }
        Refusal::Declined { name, rung } => {
            anyhow!(
                "task {name:?} not found; fetch via the {} rung declined",
                rung.name
            )
        }
        Refusal::NoCapability { provider, op } => anyhow!(
            "{} cannot {op} {task_name:?}",
            runner_providers::REGISTRY.by_id(*provider).label,
        ),
        Refusal::Ambiguous { candidates } => {
            let names: Vec<String> = candidates
                .iter()
                .map(|(_, scope)| scope.label().to_owned())
                .collect();
            super::qualify::member_ambiguity_message(task_name, &names)
        }
        Refusal::Unsafe(runner_core::Unsafe::NameShape { name, provider }) => anyhow!(
            "{} cannot take {name:?}: it is not a name that primitive accepts",
            runner_providers::REGISTRY.by_id(*provider).label,
        ),
        Refusal::Unsafe(runner_core::Unsafe::LoaderHook { name }) => {
            anyhow!("a repository config may not set {name}, which decides what code a tool loads")
        }
        Refusal::Unsafe(runner_core::Unsafe::EscapesRoot { path }) => {
            let _ = ctx;
            anyhow!("{} resolves outside the project root", path.display())
        }
    }
}

fn runtime_suggestions(
    alternatives: &[runner_core::ProviderId],
    registry: &runner_core::Registry,
) -> Vec<String> {
    alternatives
        .iter()
        .filter_map(|id| {
            JsRuntime::from_label(registry.by_id(*id).label)
                .map(|runtime| format!("--runtime {}", runtime.label()))
        })
        .collect()
}

fn runtime_origin(layer: &runner_core::Layer) -> String {
    use runner_core::Layer;
    match layer {
        Layer::Cli => "selected by --runtime".into(),
        Layer::Env => "selected by RUNNER_RUNTIME".into(),
        Layer::ConfigFile(path) => format!("selected by {}", path.display()),
        Layer::Manifest(path) => format!("declared in {}", path.display()),
        Layer::Lockfile(path) => format!("locked in {}", path.display()),
        Layer::Probe => "discovered on PATH".into(),
    }
}

/// `name` in the project's own `node_modules/.bin` dirs alone.
fn project_bin(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let bins = crate::commands::node_bin_dirs(dir);
    let search = std::env::join_paths(bins).ok()?;
    crate::resolver::probe::probe_in(name, &search, std::env::var_os("PATHEXT").as_deref())
}

#[cfg(test)]
mod tests {

    use std::process::Command;

    use super::{Dispatch, SpawnDispatch, check_make_args};
    use crate::commands::run::decision::{Observed, PmDecision};
    use crate::resolver::{OverrideOrigin, ResolutionOverrides};
    use crate::types::{JsRuntime, PackageManager, ProjectContext, Task, TaskRunner, TaskSource};

    #[test]
    fn a_locked_python_project_without_scripts_still_names_its_package_manager() {
        let dir = crate::tool::test_support::TempDir::new("python-pm-no-scripts");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("uv.lock"), "version = 1\n").unwrap();
        let ctx = crate::detect::detect(dir.path());
        let resolved = Observed::observe(&ctx, &ResolutionOverrides::default())
            .unwrap()
            .decision(runner_core::ProviderId::Pyproject)
            .expect("uv.lock names the manager");
        assert_eq!(resolved.pm, PackageManager::Uv);
        let described = resolved.describe();
        assert!(
            described.starts_with("uv via ") && described.ends_with("uv.lock"),
            "{described}"
        );
    }

    fn manifest_decision() -> PmDecision {
        PmDecision {
            pm: PackageManager::Bun,
            layer: runner_core::Layer::Manifest("package.json".into()),
            at: "package.json".into(),
            field: Some("packageManager"),
            on_fail: None,
            scope: runner_core::Scope::Root,
        }
    }

    #[test]
    fn runtime_hints_offer_only_client_supported_choices_from_the_refusal() {
        use runner_core::ProviderId;
        assert_eq!(
            super::runtime_suggestions(
                &[ProviderId::Python, ProviderId::Deno],
                &runner_providers::REGISTRY
            ),
            ["--runtime deno"]
        );
        assert!(
            super::runtime_suggestions(&[ProviderId::Python], &runner_providers::REGISTRY)
                .is_empty()
        );
        let refusal = |alternatives| runner_core::Refusal::UnsupportedFile {
            provider: ProviderId::Node,
            file: "input.widget".into(),
            reason: "widget loader unavailable",
            chosen_by: Some(runner_core::Layer::Env),
            alternatives,
        };
        let message =
            super::refusal_error(&context(), "input.widget", &refusal(vec![ProviderId::Deno]))
                .to_string();
        assert!(message.contains("--runtime deno"));
        assert!(message.contains("RUNNER_RUNTIME"));
        assert!(!message.contains("bun") && !message.contains("JSX"));
        let message =
            super::refusal_error(&context(), "input.widget", &refusal(vec![])).to_string();
        assert!(!message.contains("Try") && !message.contains("--runtime"));
    }

    fn context() -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        ProjectContext {
            cwd: root.clone(),
            root,
            package_managers: Vec::new(),
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

    fn resolve_dispatch(
        ctx: &ProjectContext,
        overrides: &ResolutionOverrides,
        task: &str,
        args: &[String],
        sink: crate::commands::WarningSink<'_>,
        allow: bool,
    ) -> anyhow::Result<Dispatch> {
        crate::tool::test_support::seed_context(ctx);
        super::resolve_dispatch(ctx, overrides, task, args, sink, allow)
    }

    fn expect_command(dispatch: Dispatch) -> Command {
        match dispatch {
            Dispatch::Spawn(spawn) => spawn.command,
            Dispatch::Builtin(_) => {
                panic!("expected a spawnable command")
            }
        }
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn resolved_pm_not_found_includes_selection_provenance() {
        let spawn = SpawnDispatch::package_manager(Command::new("bun"), manifest_decision());
        let error = spawn
            .spawn_error_with_presence(std::io::Error::from(std::io::ErrorKind::NotFound), false);
        let message = format!("{error:#}");

        assert!(message.contains(
            "bun via package.json \"packageManager\" was selected, but its executable was not \
             found on PATH",
        ));
        assert_eq!(
            error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound),
        );
    }

    #[test]
    fn resolved_pm_present_avoids_false_path_diagnosis() {
        let spawn = SpawnDispatch::package_manager(Command::new("bun"), manifest_decision());
        let error = spawn
            .spawn_error_with_presence(std::io::Error::from(std::io::ErrorKind::NotFound), true);
        let message = format!("{error:#}");

        assert!(message.contains(
            "bun via package.json \"packageManager\" was selected, but failed to launch",
        ));
        assert!(!message.contains("executable was not found on PATH"));
    }

    #[test]
    fn resolved_pm_non_not_found_preserves_io_error() {
        let spawn = SpawnDispatch::package_manager(Command::new("bun"), manifest_decision());
        let error = spawn.spawn_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let message = format!("{error:#}");

        assert!(!message.contains("was selected"));
        assert_eq!(
            error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::PermissionDenied),
        );
    }

    #[test]
    fn passthrough_not_found_preserves_io_error() {
        let spawn = SpawnDispatch::passthrough(Command::new("missing"));
        let error = spawn.spawn_error(std::io::Error::from(std::io::ErrorKind::NotFound));

        assert_eq!(
            error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound),
        );
    }

    fn justfile_task(name: &str) -> Task {
        Task {
            name: name.to_string(),
            source: TaskSource::Justfile,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
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
            detail: crate::types::TaskDetail::default(),
            member: None,
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
        // PM-exec and bun x tried to resolve it as a GitHub package spec
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
    fn a_package_spec_with_no_present_provider_is_refused_and_never_spawned_from_path() {
        let err = resolve_dispatch(
            &context(),
            &ResolutionOverrides::default(),
            "user/repo#ref",
            &[],
            None,
            true,
        )
        .expect_err("no provider can take a package spec");

        let message = format!("{err:#}");
        assert!(message.contains("not found"), "{message}");
        assert!(message.contains("exec"), "{message}");
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
    fn deno_tasks_always_dispatch_through_deno_task() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Deno);
        ctx.tasks.push(Task {
            name: "greet".to_string(),
            source: TaskSource::DenoJson,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        });

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "greet",
                &[],
                None,
                true,
            )
            .expect("deno task should dispatch"),
        );

        assert_eq!(command.get_program().to_string_lossy(), "deno");
        assert_eq!(command_args(&command), ["task", "greet"]);
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
            detail: crate::types::TaskDetail::default(),
            member: None,
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
            detail: crate::types::TaskDetail::default(),
            member: None,
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
            detail: crate::types::TaskDetail::default(),
            member: None,
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
    fn make_task_forwards_assignments_and_refuses_other_words() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        ctx.tasks.push(Task {
            name: "build".to_string(),
            source: TaskSource::Makefile,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        });

        let assignments = [String::from("CC=clang")];
        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "build",
                &assignments,
                None,
                true,
            )
            .expect("assignments reach make"),
        );
        assert_eq!(command_args(&command), ["build", "CC=clang"]);

        let flag = [String::from("--help")];
        let err = resolve_dispatch(
            &ctx,
            &ResolutionOverrides::default(),
            "build",
            &flag,
            None,
            true,
        )
        .expect_err("a flag never reaches make's parser");
        assert!(format!("{err:#}").contains("cannot take \"--help\""));
    }

    #[test]
    fn package_script_wrapping_make_rejects_flags_too() {
        let wrapper = Task {
            name: "build".to_string(),
            source: TaskSource::PackageJson,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: Some(TaskRunner::Make),
            detail: crate::types::TaskDetail::default(),
            member: None,
        };

        check_make_args(&wrapper, &[String::from("CC=clang")]).expect("assignments pass");
        let err = check_make_args(&wrapper, &[String::from("--help")])
            .expect_err("a flag appended by the package manager reaches make's parser");
        let text = format!("{err:#}");
        assert!(text.contains("package.json script \"build\""), "{text}");
        assert!(text.contains("cannot take \"--help\""), "{text}");

        let plain = Task {
            passthrough_to: None,
            ..wrapper
        };
        check_make_args(&plain, &[String::from("--help")]).expect("a real script forwards flags");
    }

    #[test]
    fn npm_prefixed_tokens_are_refused_with_the_package_form() {
        let err = resolve_dispatch(
            &context(),
            &ResolutionOverrides::default(),
            "npm:typescript",
            &[],
            None,
            true,
        )
        .expect_err("npm: spec is refused");
        assert!(
            format!("{err:#}").contains("--package typescript"),
            "{err:#}"
        );
    }

    #[test]
    fn a_selected_package_runs_its_own_declared_bin() {
        let dir = crate::tool::test_support::TempDir::new("package-selector");
        let pkg = dir.path().join("node_modules").join("typescript");
        let other = dir
            .path()
            .join("node_modules")
            .join("@typescript")
            .join("native");
        for (root, name) in [(&pkg, "typescript"), (&other, "@typescript/native")] {
            std::fs::create_dir_all(root.join("bin")).expect("bin dir");
            std::fs::write(
                root.join("package.json"),
                format!(
                    r#"{{"name":"{name}","bin":{{"tsc":"bin/tsc","tsserver":"bin/tsserver"}}}}"#
                ),
            )
            .expect("manifest");
            for bin in ["tsc", "tsserver"] {
                let file = root.join("bin").join(bin);
                std::fs::write(&file, "#!/usr/bin/env node\n").expect("bin");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
                        .expect("chmod");
                }
            }
        }
        let mut ctx = context();
        ctx.cwd = dir.path().to_path_buf();
        ctx.root = dir.path().to_path_buf();
        let overrides = ResolutionOverrides {
            package: Some("typescript".to_string()),
            ..ResolutionOverrides::default()
        };

        let command = expect_command(
            resolve_dispatch(&ctx, &overrides, "tsc", &[String::from("-v")], None, true)
                .expect("selected package dispatches"),
        );
        let argv: Vec<String> =
            std::iter::once(command.get_program().to_string_lossy().into_owned())
                .chain(command_args(&command))
                .collect();
        let expected = pkg.join("bin").join("tsc").to_string_lossy().into_owned();
        assert!(argv.contains(&expected), "{argv:?} should name {expected}");
        assert_eq!(argv.last().map(String::as_str), Some("-v"));

        let err = resolve_dispatch(&ctx, &overrides, "tsx", &[], None, true)
            .expect_err("an undeclared bin is refused");
        assert!(
            format!("{err:#}").contains("exposes tsc, tsserver"),
            "{err:#}"
        );
    }

    #[test]
    fn a_missing_selected_package_refuses_a_bin_another_package_installed() {
        let dir = crate::tool::test_support::TempDir::new("package-selector-shadow");
        let bin_dir = dir.path().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let shadow = bin_dir.join("tsx");
        std::fs::write(&shadow, "#!/usr/bin/env node\n").expect("bin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let mut ctx = context();
        ctx.cwd = dir.path().to_path_buf();
        ctx.root = dir.path().to_path_buf();
        ctx.package_managers.push(PackageManager::Npm);
        let overrides = ResolutionOverrides {
            package: Some("typescript".to_string()),
            ..ResolutionOverrides::default()
        };

        let err = resolve_dispatch(&ctx, &overrides, "tsx", &[], None, true)
            .expect_err("the fetched package cannot be told apart from the installed bin");
        assert!(
            format!("{err:#}").contains("belongs to another package"),
            "{err:#}"
        );
    }

    #[test]
    fn a_missing_selected_package_honours_the_runtime_override() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Npm);
        let overrides = ResolutionOverrides {
            package: Some("typescript".to_string()),
            runtime: Some(crate::resolver::RuntimeOverride {
                runtime: JsRuntime::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        let command = expect_command(
            resolve_dispatch(&ctx, &overrides, "tsc", &[String::from("-v")], None, true)
                .expect("the runtime's package exec dispatches"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "bun");
        assert_eq!(
            command_args(&command),
            ["x", "--bun", "--package", "typescript", "tsc", "-v"]
        );
    }

    #[test]
    fn a_missing_selected_package_takes_a_non_node_pm_override() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Cargo);
        ctx.package_managers.push(PackageManager::Uv);
        let overrides = ResolutionOverrides {
            package: Some("ruff".to_string()),
            pm: Some(crate::resolver::PmOverride {
                pm: PackageManager::Uv,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &overrides,
                "ruff",
                &[String::from("--version")],
                None,
                true,
            )
            .expect("--pm uv reaches uvx --from"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "uvx");
        assert_eq!(
            command_args(&command),
            ["--from", "ruff", "ruff", "--version"]
        );
    }

    fn package_command(
        ctx: &ProjectContext,
        pm: Option<PackageManager>,
        package: &str,
        bin: &str,
        args: &[String],
    ) -> anyhow::Result<Command> {
        let overrides = ResolutionOverrides {
            package: Some(package.into()),
            pm: pm.map(|pm| crate::resolver::PmOverride {
                pm,
                origin: OverrideOrigin::CliFlag,
            }),
            reach: runner_core::ReachPolicy::Allow,
            ..ResolutionOverrides::default()
        };
        resolve_dispatch(ctx, &overrides, bin, args, None, true).map(expect_command)
    }

    #[test]
    fn a_missing_selected_package_goes_to_the_manager_with_its_name() {
        let mut ctx = context();
        ctx.package_managers.push(PackageManager::Bun);
        let args = [String::from("-v")];
        let command = package_command(&ctx, Some(PackageManager::Bun), "typescript", "tsc", &args)
            .expect("bun has a package-selecting exec");
        assert_eq!(
            command_args(&command),
            ["x", "--package", "typescript", "tsc", "-v"]
        );

        let command = package_command(&ctx, Some(PackageManager::Npm), "typescript", "tsc", &args)
            .expect("npx has --package");
        assert_eq!(
            command_args(&command),
            ["--package", "typescript", "--", "tsc", "-v"]
        );

        let command = package_command(&ctx, Some(PackageManager::Pnpm), "typescript", "tsc", &args)
            .expect("pnpm dlx has --package");
        assert_eq!(
            command_args(&command),
            ["--package=typescript", "dlx", "tsc", "-v"]
        );

        let err = package_command(
            &ctx,
            Some(PackageManager::Cargo),
            "typescript",
            "tsc",
            &args,
        )
        .expect_err("cargo cannot select an npm package");
        assert!(format!("{err:#}").contains("--pm"), "{err:#}");
    }

    #[test]
    fn run_make_invokes_make_with_no_target() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        let args = [String::from("-j4")];

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "make",
                &args,
                None,
                true,
            )
            .expect("runner root invocation dispatches"),
        );

        assert_eq!(command.get_program().to_string_lossy(), "make");
        assert_eq!(command_args(&command), ["-j4"]);
    }

    #[test]
    fn run_make_under_runner_make_constraint_still_invokes_make() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        let overrides = ResolutionOverrides {
            runner: Some(crate::resolver::RunnerOverride {
                runner: TaskRunner::Make,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        let command = expect_command(
            resolve_dispatch(&ctx, &overrides, "make", &[], None, true)
                .expect("constraint names the invoked runner"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "make");
        assert!(command_args(&command).is_empty());
    }

    #[test]
    fn run_make_under_a_just_runner_choice_reaches_the_host_rung() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        ctx.task_runners.push(TaskRunner::Just);
        let overrides = ResolutionOverrides {
            runner: Some(crate::resolver::RunnerOverride {
                runner: TaskRunner::Just,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        let command = expect_command(
            resolve_dispatch(&ctx, &overrides, "make", &[], None, true)
                .expect("a runner choice pins task candidates only"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "make");
        assert!(command_args(&command).is_empty());
    }

    #[test]
    fn run_make_from_a_member_runs_at_the_workspace_root() {
        let mut ctx = context();
        ctx.cwd = ctx.root.join("apps/web");
        std::fs::create_dir_all(&ctx.cwd).unwrap();
        ctx.task_runners.push(TaskRunner::Make);

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "make",
                &[],
                None,
                true,
            )
            .expect("runner root invocation dispatches"),
        );

        assert_eq!(command.get_current_dir(), Some(ctx.root.as_path()));
    }

    #[test]
    fn run_make_under_a_prefer_list_without_make_reaches_the_host_rung() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        ctx.task_runners.push(TaskRunner::Just);
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };

        let command = expect_command(
            resolve_dispatch(&ctx, &overrides, "make", &[], None, true)
                .expect("a prefer list ranks and never restricts"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "make");
    }

    #[test]
    fn run_make_under_a_prefer_list_naming_make_invokes_make() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        ctx.task_runners.push(TaskRunner::Just);
        let listed = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just, TaskRunner::Make],
            ..ResolutionOverrides::default()
        };
        let command = expect_command(
            resolve_dispatch(&ctx, &listed, "make", &[], None, true)
                .expect("a prefer list naming make admits it"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "make");
    }

    #[test]
    fn root_invocation_records_a_task_frame_and_env_layers() {
        use std::ffi::OsStr;

        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Make);
        let mut overrides = ResolutionOverrides::default();
        overrides.env.tool.insert(
            "make".to_string(),
            std::collections::BTreeMap::from([("CC".to_string(), "clang".to_string())]),
        );

        let command = expect_command(
            resolve_dispatch(&ctx, &overrides, "make", &[], None, true)
                .expect("runner root invocation dispatches"),
        );

        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        assert!(
            envs.iter()
                .any(|(key, _)| *key == OsStr::new(crate::commands::TASK_STACK_ENV)),
            "the root invocation joins the recursion stack: {envs:?}"
        );
        assert!(
            envs.iter()
                .any(|(key, value)| *key == OsStr::new("CC")
                    && *value == Some(OsStr::new("clang"))),
            "[tools.make].env applies to the root invocation: {envs:?}"
        );
    }

    #[test]
    fn a_task_named_after_the_runner_wins_over_the_root_invocation() {
        let mut ctx = context();
        ctx.task_runners.push(TaskRunner::Just);
        ctx.tasks.push(justfile_task("just"));

        let command = expect_command(
            resolve_dispatch(
                &ctx,
                &ResolutionOverrides::default(),
                "just",
                &[],
                None,
                true,
            )
            .expect("task dispatches"),
        );
        assert_eq!(command.get_program().to_string_lossy(), "just");
        assert_eq!(command_args(&command), ["just"]);
    }
}
