//! The current detector's answers as the core's types.
//!
//! Detection does not record where it found each signal yet, so a provider
//! it reports carries one `Weight::Present` at the project root: the fact
//! that the detector concluded the provider is part of this tree. Step 7
//! replaces this module with `observe`.

use std::path::PathBuf;

use runner_core::{
    BinDirs, Choice, Ecosystem as CoreEcosystem, Evidence, Layer, PerEcosystem, Policy, Present,
    Project, ProviderId, Scope, SignalId, Task as CoreTask, Tree, Verbosity, Weight,
};
use runner_providers::REGISTRY;

use crate::resolver::ResolutionOverrides;
use crate::types::{Ecosystem, JsRuntime, ProjectContext, Task, TaskRunner, TaskSource};

/// The provider `label` names.
pub(crate) fn provider(label: &str) -> Option<ProviderId> {
    REGISTRY.by_label(label).map(|found| found.id)
}

/// The provider a task source is read by.
pub(crate) fn source_provider(source: TaskSource) -> Option<ProviderId> {
    provider(source.label())
}

/// The core's view of the tree runner was invoked for.
pub(crate) fn tree(ctx: &ProjectContext) -> Tree {
    Tree {
        cwd: ctx.cwd.clone(),
        root: ctx.root.clone(),
        members: ctx
            .workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .members
                    .iter()
                    .map(|member| Scope::Member {
                        name: member.label.clone(),
                        dir: member.dir.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The core's view of what the detector found, plus every provider policy
/// named. A provider the user named is part of the project whether or not
/// detection saw it; a missing binary fails at the spawn, naming itself.
pub(crate) fn project_under(ctx: &ProjectContext, policy: &Policy) -> Project {
    let mut found = project(ctx);
    let chosen: Vec<&Choice> = policy
        .runtime
        .iter()
        .chain(policy.pm.0.values())
        .chain(policy.runner.iter())
        .collect();
    for choice in chosen {
        if found.present.iter().any(|seen| seen.provider == choice.id) {
            continue;
        }
        if REGISTRY.by_id(choice.id).program.is_none() {
            continue;
        }
        found.present.insert(
            0,
            Present {
                provider: choice.id,
                scope: Scope::Root,
                version: None,
                bin_dirs: bin_dirs(choice.id, &ctx.root),
                because: vec![Evidence {
                    provider: Some(choice.id),
                    signal: Some(SignalId(0)),
                    at: ctx.root.clone(),
                    scope: Scope::Root,
                    weight: Weight::Declared,
                    declared: None,
                }],
            },
        );
    }
    let tree = tree(ctx);
    let evidence: Vec<_> = found
        .present
        .iter()
        .flat_map(|p| p.because.iter().cloned())
        .collect();
    for provider in REGISTRY.iter() {
        if let Some(hook) = provider.hooks.after_observe {
            for evidence in hook(&tree, &evidence) {
                if let Some(present) = found
                    .present
                    .iter_mut()
                    .find(|p| Some(p.provider) == evidence.provider && p.scope == evidence.scope)
                {
                    present.because.push(evidence);
                }
            }
        }
    }
    found
}

/// The core's view of what the detector found.
pub(crate) fn project(ctx: &ProjectContext) -> Project {
    let mut present: Vec<Present> = Vec::new();
    let mut add = |id: ProviderId| {
        if present.iter().any(|seen| seen.provider == id) {
            return;
        }
        present.push(Present {
            provider: id,
            scope: Scope::Root,
            version: None,
            bin_dirs: bin_dirs(id, &ctx.root),
            because: vec![Evidence {
                provider: Some(id),
                signal: Some(SignalId(0)),
                at: ctx.root.clone(),
                scope: Scope::Root,
                weight: Weight::Present,
                declared: None,
            }],
        });
    };
    for pm in &ctx.package_managers {
        if let Some(id) = provider(pm.label()) {
            add(id);
        }
    }
    for runner in &ctx.task_runners {
        if let Some(id) = provider(runner.label()) {
            add(id);
        }
    }
    for task in &ctx.tasks {
        if let Some(id) = source_provider(task.source) {
            add(id);
        }
    }
    Project {
        present,
        tasks: ctx.tasks.iter().filter_map(task).collect(),
        warnings: Vec::new(),
    }
}

/// Where a provider's installed executables live under `root`.
fn bin_dirs(id: ProviderId, root: &std::path::Path) -> Vec<PathBuf> {
    match REGISTRY.by_id(id).caps.bins.map(|bins| bins.dirs) {
        Some(BinDirs::Static(dirs)) => dirs.iter().map(|dir| root.join(dir)).collect(),
        Some(BinDirs::Ask(ask)) => ask(root),
        None => Vec::new(),
    }
}

/// The core's view of one task.
pub(crate) fn task(task: &Task) -> Option<CoreTask> {
    Some(CoreTask {
        name: task.name.clone(),
        source: source_provider(task.source)?,
        scope: task
            .member
            .as_ref()
            .map_or(Scope::Root, |member| Scope::Member {
                name: member.label.clone(),
                dir: member.dir.clone(),
            }),
        target: task.run_target.clone(),
        description: task.description.clone(),
        alias_of: task.alias_of.clone(),
        forwards_to: task.passthrough_to.and_then(|to| provider(to.label())),
        detail: runner_core::TaskDetail::default(),
    })
}

/// The core's view of the override chain.
pub(crate) fn policy(overrides: &ResolutionOverrides) -> Policy {
    let mut pm = PerEcosystem::default();
    for (ecosystem, chosen) in &overrides.pm_by_ecosystem {
        if let Some(id) = provider(chosen.pm.label()) {
            pm.0.insert(
                ecosystem_of(*ecosystem),
                Choice {
                    id,
                    from: layer(&chosen.origin),
                },
            );
        }
    }
    if let Some(chosen) = overrides.pm.as_ref()
        && let Some(id) = provider(chosen.pm.label())
    {
        pm.0.insert(
            ecosystem_of(chosen.pm.ecosystem()),
            Choice {
                id,
                from: layer(&chosen.origin),
            },
        );
    }
    Policy {
        prefer: overrides
            .prefer_sources
            .iter()
            .copied()
            .chain(
                overrides
                    .prefer_runners
                    .iter()
                    .filter_map(|runner| runner.task_source()),
            )
            .filter_map(source_provider)
            .collect(),
        task_sources: overrides
            .task_source_overrides
            .iter()
            .map(|(name, sources)| {
                (
                    name.clone(),
                    sources
                        .iter()
                        .copied()
                        .filter_map(source_provider)
                        .collect(),
                )
            })
            .collect(),
        pm,
        runner: overrides
            .runner
            .as_ref()
            .and_then(|chosen| runner_choice(chosen.runner, &chosen.origin)),
        runtime: overrides
            .runtime
            .as_ref()
            .and_then(|chosen| runtime_choice(chosen.runtime, &chosen.origin)),
        frozen: false,
        scripts: runner_core::ScriptPolicy::Default,
        reach: overrides.reach,
        verbosity: verbosity(overrides),
        host_stderr: false,
        env: env_layers(overrides),
        tool_ops: tool_ops(overrides),
        trust: runner_core::TrustPolicy::Project,
    }
}

/// Select task metadata through the same core selector used by dispatch.
pub(crate) fn selected<'a>(
    ctx: &'a ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
) -> Option<&'a Task> {
    let tree = tree(ctx);
    let policy = policy(overrides);
    let project = project_under(ctx, &policy);
    let cascade = runner_core::Cascade {
        tree: &tree,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    let selected = runner_core::select(&cascade, token).ok().flatten()?;
    ctx.tasks
        .iter()
        .find(|entry| task(entry).as_ref() == Some(selected))
}

/// Inputs shared by execution and explanation while the detector/resolver migrate.
pub(crate) struct Prepared {
    pub tree: Tree,
    pub policy: Policy,
    pub project: Project,
    pub node: Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    pub requested: crate::tool::HostVerbosity,
}

pub(crate) fn prepare(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
) -> Prepared {
    let tree = tree(ctx);
    let mut policy = policy(overrides);
    let node = crate::resolver::Resolver::new(ctx, overrides).resolve_node_pm();
    if let Ok(decision) = &node
        && let Some(id) = provider(decision.pm.label())
    {
        policy
            .pm
            .0
            .entry(ecosystem_of(decision.pm.ecosystem()))
            .or_insert(Choice {
                id,
                from: Layer::Probe,
            });
    }
    let key =
        selected(ctx, overrides, token).map_or_else(|| token.to_owned(), super::task_output_key);
    let requested = overrides.host_verbosity_for(&key);
    policy.host_stderr = requested.stream == crate::tool::Stream::Stderr;
    policy.verbosity = match requested.diagnostics {
        crate::tool::HostDiagnostics::Normal => Verbosity::Normal,
        crate::tool::HostDiagnostics::Quiet => Verbosity::Quiet,
        crate::tool::HostDiagnostics::Reduced => Verbosity::VeryQuiet,
    };
    if overrides.explain {
        policy.reach = runner_core::ReachPolicy::Allow;
    }
    let project = project_under(ctx, &policy);
    Prepared {
        tree,
        policy,
        project,
        node,
        requested,
    }
}

impl Prepared {
    pub(crate) fn cascade<'a>(
        &'a self,
        dep: &'a runner_core::DepFn<'a>,
        confirm: Option<&'a runner_core::ConfirmFn<'a>>,
    ) -> runner_core::Cascade<'a> {
        runner_core::Cascade {
            tree: &self.tree,
            project: &self.project,
            policy: &self.policy,
            registry: &REGISTRY,
            builtins: &["install", "clean", "list", "info", "completions"],
            dep: Some(dep),
            confirm,
        }
    }
}

/// Plan a token without authorizing or executing it, for reports.
pub(crate) fn preview(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
) -> Result<(runner_core::Rung, runner_core::Dispatch), runner_core::Refusal> {
    let mut prepared = prepare(ctx, overrides, token);
    prepared.policy.reach = runner_core::ReachPolicy::Allow;
    let dep = |name: &str| {
        super::local_dep::installed_binary(ctx, name)
            .map_err(|error| runner_core::Refusal::Invalid(error.to_string()))
    };
    runner_core::dispatch(&prepared.cascade(&dep, None), token, &[])
}

/// The layer an override came from.
fn layer(origin: &crate::resolver::OverrideOrigin) -> Layer {
    match origin {
        crate::resolver::OverrideOrigin::CliFlag => Layer::Cli,
        crate::resolver::OverrideOrigin::EnvVar => Layer::Env,
        crate::resolver::OverrideOrigin::ConfigFile { path } => Layer::ConfigFile(path.clone()),
    }
}

fn runner_choice(runner: TaskRunner, origin: &crate::resolver::OverrideOrigin) -> Option<Choice> {
    let source = runner.task_source()?;
    Some(Choice {
        id: source_provider(source)?,
        from: layer(origin),
    })
}

fn runtime_choice(runtime: JsRuntime, origin: &crate::resolver::OverrideOrigin) -> Option<Choice> {
    Some(Choice {
        id: provider(runtime.label())?,
        from: layer(origin),
    })
}

/// The core ecosystem a cli one names.
fn ecosystem_of(ecosystem: Ecosystem) -> CoreEcosystem {
    CoreEcosystem::ALL
        .into_iter()
        .find(|core| core.label() == ecosystem.label())
        .unwrap_or(CoreEcosystem::Any)
}

/// The host diagnostic level the invocation asked for.
const fn verbosity(overrides: &ResolutionOverrides) -> Verbosity {
    match overrides.output_policy.host_diagnostics {
        crate::tool::HostDiagnostics::Normal => Verbosity::Normal,
        crate::tool::HostDiagnostics::Quiet => Verbosity::Quiet,
        crate::tool::HostDiagnostics::Reduced => Verbosity::VeryQuiet,
    }
}

/// The env layers, with the tool layer keyed by provider.
fn env_layers(overrides: &ResolutionOverrides) -> runner_core::EnvLayers {
    let mut layers = runner_core::EnvLayers {
        project: overrides.env.project.clone(),
        ..runner_core::EnvLayers::default()
    };
    for (label, table) in &overrides.env.tool {
        if let Some(id) = provider(label) {
            layers.tool.insert(id, table.clone());
        }
    }
    layers.task = overrides.env.task.clone();
    layers
}

/// The operations each tool manager runs on install.
fn tool_ops(
    overrides: &ResolutionOverrides,
) -> std::collections::BTreeMap<ProviderId, Vec<String>> {
    overrides
        .tool_install
        .iter()
        .filter_map(|(label, operations)| Some((provider(label)?, operations.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use runner_core::{ProviderId, Scope};

    use super::{policy, project, source_provider, tree};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{PackageManager, ProjectContext, Task, TaskRunner, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("/p"),
            root: PathBuf::from("/p"),
            package_managers: vec![PackageManager::Pnpm],
            task_runners: vec![TaskRunner::Just],
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
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

    #[test]
    fn every_task_source_the_cli_models_names_a_provider() {
        for source in [
            TaskSource::PackageJson,
            TaskSource::Makefile,
            TaskSource::Justfile,
            TaskSource::Taskfile,
            TaskSource::TurboJson,
            TaskSource::DenoJson,
            TaskSource::CargoAliases,
            TaskSource::GoPackage,
            TaskSource::BaconToml,
            TaskSource::MiseToml,
            TaskSource::PyprojectScripts,
        ] {
            assert!(
                source_provider(source).is_some(),
                "{} names no provider",
                source.label()
            );
        }
    }

    #[test]
    fn detected_tools_and_task_sources_all_become_present_providers() {
        let ctx = context(vec![task("build", TaskSource::PackageJson)]);
        let found = project(&ctx);
        let ids: Vec<ProviderId> = found.present.iter().map(|p| p.provider).collect();
        assert!(ids.contains(&ProviderId::Pnpm));
        assert!(ids.contains(&ProviderId::Just));
        assert!(ids.contains(&ProviderId::PackageJson));
        assert!(
            found.present.iter().all(|p| !p.because.is_empty()),
            "every present provider carries evidence"
        );
        assert_eq!(found.tasks.len(), 1);
        assert_eq!(found.tasks[0].source, ProviderId::PackageJson);
        assert_eq!(found.tasks[0].scope, Scope::Root);
        assert_eq!(
            found
                .present
                .iter()
                .find(|p| p.provider == ProviderId::Pnpm)
                .map(|p| p.bin_dirs.clone()),
            Some(vec![PathBuf::from("/p/node_modules/.bin")])
        );
    }

    #[test]
    fn a_workspace_member_becomes_a_member_scope() {
        let mut ctx = context(Vec::new());
        let member = crate::types::WorkspaceMember {
            name: "@acme/web".to_string(),
            path: "apps/web".to_string(),
            label: "@acme/web".to_string(),
            dir: PathBuf::from("/p/apps/web"),
        };
        ctx.workspace = Some(crate::types::Workspace {
            root: PathBuf::from("/p"),
            kinds: Vec::new(),
            members: vec![std::sync::Arc::new(member)],
            current: None,
        });
        assert_eq!(
            tree(&ctx).members,
            [Scope::Member {
                name: "@acme/web".to_string(),
                dir: PathBuf::from("/p/apps/web"),
            }]
        );
    }

    #[test]
    fn the_fetch_policy_reaches_the_core_policy() {
        let overrides = ResolutionOverrides {
            reach: runner_core::ReachPolicy::Local,
            ..ResolutionOverrides::default()
        };
        assert_eq!(policy(&overrides).reach, runner_core::ReachPolicy::Local);
    }
}
