//! CLI invocation inputs for observation, resolution and planning.

use runner_core::{
    Choice, Ecosystem as CoreEcosystem, Layer, PerEcosystem, Policy, Project, ProviderId, Scope,
    Task as CoreTask, Tree, Verbosity,
};
use runner_providers::REGISTRY;

const BUILTINS: &[&str] = &["install", "clean", "list", "info", "completions"];

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

/// Observe provider facts in every scope before resolving policy.
pub(crate) fn project_under(ctx: &ProjectContext, policy: &Policy) -> std::io::Result<Project> {
    let tree = tree(ctx);
    let evidence = runner_core::observe::observe(&tree, &REGISTRY)?;
    let mut project = runner_core::resolve::resolve_presence(&tree, evidence, policy, &REGISTRY)?;
    project.tasks = ctx.tasks.iter().filter_map(task).collect();
    Ok(project)
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
        detail: task.detail.clone(),
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

fn selected_in<'a>(
    ctx: &'a ProjectContext,
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    token: &str,
) -> Result<Option<&'a Task>, runner_core::Refusal> {
    let cascade = runner_core::Cascade {
        tree,
        project,
        policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    let Some(selected) = runner_core::select(&cascade, token)? else {
        return Ok(None);
    };
    Ok(ctx
        .tasks
        .iter()
        .find(|entry| task(entry).as_ref() == Some(selected)))
}

/// Inputs shared by execution and explanation while the detector/resolver migrate.
pub(crate) struct Prepared {
    pub tree: Tree,
    pub policy: Policy,
    pub project: Project,
    pub node: Result<crate::resolver::ResolvedPm, crate::resolver::ResolveError>,
    pub requested: crate::tool::HostVerbosity,
    pub task_key: String,
}

pub(crate) fn prepare(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
) -> Result<Prepared, runner_core::Refusal> {
    let tree = tree(ctx);
    let mut policy = policy(overrides);
    let project = project_under(ctx, &policy)?;
    let node = crate::resolver::Resolver::new(ctx, overrides).resolve_node_pm_in(&project);
    let key = if BUILTINS.contains(&token) {
        token.to_owned()
    } else {
        match selected_in(ctx, &tree, &project, &policy, token) {
            Ok(selected) => selected.map_or_else(|| token.to_owned(), super::task_output_key),
            Err(runner_core::Refusal::NotFound { .. } | runner_core::Refusal::Ambiguous { .. }) => {
                token.to_owned()
            }
            Err(error) => return Err(error),
        }
    };
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
    Ok(Prepared {
        tree,
        policy,
        project,
        node,
        requested,
        task_key: key,
    })
}

impl Prepared {
    pub(crate) fn selected<'a>(
        &self,
        ctx: &'a ProjectContext,
        token: &str,
    ) -> Result<Option<&'a Task>, runner_core::Refusal> {
        selected_in(ctx, &self.tree, &self.project, &self.policy, token)
    }

    pub(crate) fn preview(
        &self,
        ctx: &ProjectContext,
        overrides: &ResolutionOverrides,
        token: &str,
    ) -> Result<(runner_core::Rung, runner_core::Dispatch), runner_core::Refusal> {
        let mut policy = self.policy.clone();
        policy.reach = runner_core::ReachPolicy::Allow;
        let key = match self.selected(ctx, token) {
            Ok(selected) => selected.map_or_else(|| token.to_owned(), super::task_output_key),
            Err(runner_core::Refusal::NotFound { .. } | runner_core::Refusal::Ambiguous { .. }) => {
                token.to_owned()
            }
            Err(error) => return Err(error),
        };
        let requested = overrides.host_verbosity_for(&key);
        policy.host_stderr = requested.stream == crate::tool::Stream::Stderr;
        policy.verbosity = match requested.diagnostics {
            crate::tool::HostDiagnostics::Normal => Verbosity::Normal,
            crate::tool::HostDiagnostics::Quiet => Verbosity::Quiet,
            crate::tool::HostDiagnostics::Reduced => Verbosity::VeryQuiet,
        };
        let dep = |name: &str| {
            super::local_dep::installed_binary(ctx, name).map_err(|error| {
                match error.downcast::<std::io::Error>() {
                    Ok(error) => error.into(),
                    Err(error) => runner_core::Refusal::Invalid(error.to_string()),
                }
            })
        };
        let mut cascade = self.cascade(&dep, None);
        cascade.policy = &policy;
        let (rung, mut dispatch) = runner_core::dispatch(&cascade, token, &[])?;
        if let runner_core::Dispatch::Plan(plan) = &mut dispatch {
            let entry = if rung.name == "task" {
                self.selected(ctx, token)?
            } else {
                None
            };
            self.validate_task(entry, overrides)?;
            let mut warnings = std::collections::HashSet::new();
            super::dispatch::complete_plan(
                ctx,
                overrides,
                token,
                &[],
                rung,
                entry,
                &key,
                plan,
                Some(&mut warnings),
            )
            .map_err(|error| match error.downcast::<std::io::Error>() {
                Ok(error) => error.into(),
                Err(error) => runner_core::Refusal::Invalid(error.to_string()),
            })?;
        }
        Ok((rung, dispatch))
    }

    pub(crate) fn validate_task(
        &self,
        entry: Option<&Task>,
        overrides: &ResolutionOverrides,
    ) -> Result<(), runner_core::Refusal> {
        if entry.is_some_and(|task| task.source == TaskSource::PackageJson)
            && overrides.runtime.is_none()
        {
            self.node
                .as_ref()
                .map_err(|error| runner_core::Refusal::Invalid(error.to_string()))?;
        }
        Ok(())
    }

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
            builtins: BUILTINS,
            dep: Some(dep),
            confirm,
        }
    }
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

    use super::{policy, project_under, source_provider, tree};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{PackageManager, ProjectContext, Task, TaskRunner, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        let ctx = ProjectContext {
            cwd: root.clone(),
            root,
            package_managers: vec![PackageManager::Pnpm],
            task_runners: vec![TaskRunner::Just],
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        };
        crate::tool::test_support::seed_context(&ctx);
        ctx
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
        let found = project_under(&ctx, &runner_core::Policy::default()).unwrap();
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
            Some(vec![ctx.root.join("node_modules/.bin")])
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
            root: ctx.root.clone(),
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

    #[test]
    fn selected_metadata_and_dispatch_share_prepared_policy_and_evidence() {
        use crate::resolver::{OverrideOrigin, RuntimeOverride};
        let dir = crate::tool::test_support::TempDir::new("selection-context");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"packageManager":"yarn@4.0.0"}"#,
        )
        .unwrap();
        let mut ctx = context(vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::Justfile),
        ]);
        ctx.root = dir.path().to_owned();
        ctx.cwd = ctx.root.clone();
        ctx.package_managers = vec![PackageManager::Yarn];
        let overrides = ResolutionOverrides {
            runtime: Some(RuntimeOverride {
                runtime: crate::types::JsRuntime::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };
        let prepared = super::prepare(&ctx, &overrides, "build").unwrap();
        assert!(
            prepared
                .project
                .present
                .iter()
                .any(|p| p.provider == ProviderId::Bun)
        );
        assert!(prepared.project.present.iter().flat_map(|p| &p.because)
            .any(|e| matches!(&e.declared, Some(runner_core::Declared::Variant(v)) if v == "berry")));
        let dep = |_: &str| Ok(None);
        let cascade = prepared.cascade(&dep, None);
        let selected = runner_core::select(&cascade, "build").unwrap().unwrap();
        std::fs::write(ctx.root.join("package.json"), "{ invalid").unwrap();
        let metadata = prepared.selected(&ctx, "build").unwrap().unwrap();
        assert!(prepared.preview(&ctx, &overrides, "build").is_ok());
        assert!(super::prepare(&ctx, &overrides, "build").is_err());
        assert_eq!(super::task(metadata).as_ref(), Some(selected));
        assert_eq!(prepared.task_key, super::super::task_output_key(metadata));
        let (_, runner_core::Dispatch::Plan(plan)) =
            runner_core::dispatch(&cascade, "build", &[]).unwrap()
        else {
            panic!("expected task plan");
        };
        assert_eq!(plan.scope, selected.scope);
        assert!(plan.argv.iter().any(|arg| arg == "build"));
    }

    #[test]
    fn preview_preserves_observation_error_kind() {
        let dir = crate::tool::test_support::TempDir::new("preview-error");
        let path = dir.path().join("package.json");
        std::fs::create_dir(&path).unwrap();
        let expected = std::fs::read_to_string(&path).unwrap_err().kind();
        let mut ctx = context(Vec::new());
        ctx.root = dir.path().to_owned();
        ctx.cwd = ctx.root.clone();
        ctx.package_managers = vec![PackageManager::Yarn];
        let overrides = ResolutionOverrides::default();
        for error in [
            super::prepare(&ctx, &overrides, "build")
                .and_then(|p| p.preview(&ctx, &overrides, "build"))
                .unwrap_err(),
            super::prepare(&ctx, &overrides, "build")
                .and_then(|p| p.selected(&ctx, "build"))
                .unwrap_err(),
        ] {
            let runner_core::Refusal::Observation { kind, message } = error else {
                panic!("observation refusal");
            };
            assert_eq!(kind, expected);
            assert!(message.contains("package.json") && message.contains("yarn"));
        }
    }
}
