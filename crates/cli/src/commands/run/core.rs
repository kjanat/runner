//! CLI invocation inputs for observation, resolution and planning.

use crate::provider::Named;
use runner_core::{
    Choice, Ecosystem as CoreEcosystem, Layer, PerEcosystem, Policy, Project, ProviderId, Scope,
    Task as CoreTask, TaskRank, Tree, Verbosity,
};
use runner_providers::REGISTRY;

pub(crate) const BUILTINS: &[&str] = &["install", "clean", "list", "info", "completions"];

use crate::resolver::ResolutionOverrides;
use crate::types::{ProjectContext, Task};
use runner_core::Ecosystem;

/// The provider `label` names.
pub(crate) fn provider(label: &str) -> Option<ProviderId> {
    REGISTRY.by_label(label).map(|found| found.id)
}

/// The provider a task source is read by.
pub(crate) fn source_provider(source: ProviderId) -> Option<ProviderId> {
    provider(source.label())
}

/// The core's view of the tree runner was invoked for.
pub(crate) fn tree(ctx: &ProjectContext) -> Tree {
    Tree {
        cwd: ctx.cwd.clone(),
        root: ctx.root.clone(),
        members: ctx.workspace.as_ref().map_or_default(|workspace| {
            workspace
                .members
                .iter()
                .map(|member| Scope::Member {
                    name: member.label.clone(),
                    dir: member.dir.clone(),
                })
                .collect()
        }),
    }
}

/// Every provider's evidence in `tree`, with lockfiles the repository does
/// not track outranked by one it does.
pub(crate) fn observe_evidence(tree: &Tree) -> std::io::Result<Vec<runner_core::Evidence>> {
    let mut evidence = runner_core::observe::observe(tree, &REGISTRY)?;
    runner_core::prefer_tracked_lockfiles(&mut evidence, &REGISTRY, &crate::tool::git::is_tracked);
    Ok(evidence)
}

/// The project `ctx` observed and resolved, or the failure that stopped it.
pub(crate) fn project(ctx: &ProjectContext) -> std::io::Result<Project> {
    ctx.project
        .clone()
        .map_err(|unobserved| (&unobserved).into())
}

/// The argument spec `task`'s source declares for it.
///
/// # Errors
/// Returns the source's failure to read the spec.
pub(crate) fn usage(
    ctx: &ProjectContext,
    task: &Task,
) -> Result<Option<runner_core::UsageSpec>, runner_core::Warning> {
    let (Some(task), Ok(project)) = (self::task(task), ctx.project.as_ref()) else {
        return Ok(None);
    };
    runner_core::usage(&tree(ctx), project, &task, &REGISTRY)
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

/// The core's view of the settings for the task `key` names, or for the
/// invocation as a whole with `None`.
pub(crate) fn policy(overrides: &ResolutionOverrides, key: Option<&str>) -> Policy {
    let task = key.map_or_default(|key| overrides.task(key));
    let config = |task_key: &str| {
        overrides
            .config
            .as_ref()
            .map(|path| crate::resolver::OverrideOrigin::TaskConfig {
                path: path.clone(),
                task: task_key.to_owned(),
            })
    };
    let mut pm = PerEcosystem::default();
    let chosen_pm = overrides
        .pm
        .as_ref()
        .map(|chosen| (chosen.pm, chosen.origin.clone()))
        .or_else(|| Some((task.pm?, config(key?)?)));
    if let Some((chosen, origin)) = chosen_pm
        && let Some(id) = provider(chosen.label())
    {
        pm.0.insert(
            ecosystem_of(chosen.ecosystem()),
            Choice {
                id,
                from: layer(&origin),
            },
        );
    }
    let source = overrides
        .source
        .as_ref()
        .map(|chosen| (chosen.source, chosen.origin.clone()))
        .or_else(|| Some((task.source?, config(key?)?)))
        .and_then(|(source, origin)| {
            Some(Choice {
                id: source_provider(source)?,
                from: layer(&origin),
            })
        });
    Policy {
        pm,
        source,
        runtime: key
            .map_or_else(
                || overrides.runtime.clone(),
                |key| overrides.runtime_for(key),
            )
            .and_then(|chosen| runtime_choice(chosen.runtime, &chosen.origin)),
        named: overrides
            .tasks
            .values()
            .flat_map(|task| [task.pm, task.runtime])
            .flatten()
            .filter_map(|id| provider(id.label()))
            .collect(),
        frozen: false,
        scripts: runner_core::ScriptPolicy::Default,
        download: download(overrides),
        verbosity: verbosity_of(overrides.output_for(key).tool),
        env: env_layers(overrides),
        trust: runner_core::TrustPolicy::Project,
    }
}

/// The core's download policy.
pub(crate) const fn download(overrides: &ResolutionOverrides) -> runner_core::Download {
    match overrides.download.value {
        crate::config::Download::Allow => runner_core::Download::Allow,
        crate::config::Download::Refuse => runner_core::Download::Refuse,
        crate::config::Download::Ask => runner_core::Download::Ask,
    }
}

pub(super) fn selected_in<'a>(
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

/// The tasks `token` addresses in its nearest scope, lowest rank first, as
/// `ctx` holds them.
///
/// # Errors
///
/// Returns the selection refusal.
pub(crate) fn ranked_in<'a>(
    ctx: &'a ProjectContext,
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    token: &str,
) -> Result<Vec<(&'a Task, TaskRank)>, runner_core::Refusal> {
    let cascade = runner_core::Cascade {
        tree,
        project,
        policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    Ok(runner_core::ranked_tasks(&cascade, token)?
        .into_iter()
        .filter_map(|(ranked, rank)| {
            ctx.tasks
                .iter()
                .find(|entry| task(entry).as_ref() == Some(ranked))
                .map(|entry| (entry, rank))
        })
        .collect())
}

/// Why the first of `ranked` outranks the second, from the first rank field
/// that separates them.
pub(crate) fn rank_reason(ranked: &[(&Task, TaskRank)]) -> &'static str {
    let [(_, first), (_, second), ..] = ranked else {
        return "it is the only candidate";
    };
    if first.tier != second.tier {
        "the chosen package manager or runtime dispatches it"
    } else if first.by_package_manager != second.by_package_manager {
        "the chosen runtime dispatches it"
    } else if first.dispatch_order != second.dispatch_order {
        "the dispatching provider lists its source first"
    } else if first.priority != second.priority {
        "its source has the higher task priority"
    } else if first.source != second.source {
        "provider order breaks the tie"
    } else {
        "a recipe outranks an alias"
    }
}

/// Inputs shared by execution and explanation while the detector migrates.
pub(crate) struct Prepared {
    pub tree: Tree,
    pub policy: Policy,
    pub project: Project,
    pub requested: crate::tool::HostVerbosity,
}

pub(crate) fn prepare(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
) -> Result<Prepared, runner_core::Refusal> {
    let tree = tree(ctx);
    let project = project(ctx)?;
    let key = if BUILTINS.contains(&token) {
        token.to_owned()
    } else {
        task_key(ctx, &tree, &project, &policy(overrides, Some(token)), token)?
    };
    let mut policy = policy(overrides, Some(&key));
    let requested = overrides.host_verbosity_for(&key);
    if overrides.dry_run {
        policy.download = runner_core::Download::Allow;
    }
    Ok(Prepared {
        tree,
        policy,
        project,
        requested,
    })
}

impl Prepared {
    /// The package manager that dispatches `source` in the invocation scope.
    pub(crate) fn decision(&self, source: ProviderId) -> Option<super::decision::PmDecision> {
        super::decision::decide(&self.tree, &self.project, &self.policy, source)
    }

    /// The package manager that dispatches `selected` in its own scope when
    /// package managers dispatch its source, else the first one that
    /// dispatches a managed source in that scope.
    pub(crate) fn decision_for(
        &self,
        selected: Option<&Task>,
    ) -> Option<super::decision::PmDecision> {
        let selected = selected.and_then(task);
        let scope = selected.as_ref().map_or_else(
            || runner_core::plan::scope_at(&self.tree, &self.tree.cwd),
            |task| task.scope.clone(),
        );
        if let Some(task) = &selected
            && task.source.is_managed()
        {
            return self.decision_in(task.source, &scope);
        }
        crate::provider::managed_sources()
            .into_iter()
            .find_map(|source| self.decision_in(source, &scope))
    }

    /// The package manager that dispatches `source` in `scope`.
    pub(crate) fn decision_in(
        &self,
        source: ProviderId,
        scope: &Scope,
    ) -> Option<super::decision::PmDecision> {
        super::decision::decide_in(&self.project, &self.policy, source, scope)
    }

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
        let key = task_key(ctx, &self.tree, &self.project, &self.policy, token)?;
        let mut policy = policy(overrides, Some(&key));
        policy.download = runner_core::Download::Allow;
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
            let mut warnings = std::collections::HashSet::new();
            super::dispatch::complete_plan(
                ctx,
                overrides,
                &super::dispatch::Chosen {
                    token,
                    rung,
                    entry,
                    key: &key,
                },
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
        crate::resolver::OverrideOrigin::ConfigFile { path }
        | crate::resolver::OverrideOrigin::TaskConfig { path, .. } => {
            Layer::ConfigFile(path.clone())
        }
    }
}

fn runtime_choice(runtime: ProviderId, origin: &crate::resolver::OverrideOrigin) -> Option<Choice> {
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

const fn verbosity_of(diagnostics: crate::tool::HostDiagnostics) -> Verbosity {
    match diagnostics {
        crate::tool::HostDiagnostics::Normal => Verbosity::Normal,
        crate::tool::HostDiagnostics::Quiet => Verbosity::Quiet,
        crate::tool::HostDiagnostics::Reduced => Verbosity::VeryQuiet,
    }
}

/// The `[tasks.<key>]` identity `token` selects, or the token itself.
fn task_key(
    ctx: &ProjectContext,
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    token: &str,
) -> Result<String, runner_core::Refusal> {
    match selected_in(ctx, tree, project, policy, token) {
        Ok(selected) => Ok(selected.map_or_else(|| token.to_owned(), super::task_output_key)),
        Err(
            runner_core::Refusal::NotFound { .. }
            | runner_core::Refusal::Ambiguous { .. }
            | runner_core::Refusal::NoSourceTask { .. },
        ) => Ok(token.to_owned()),
        Err(error) => Err(error),
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

#[cfg(test)]
mod tests {
    use crate::provider::Named;
    use std::path::PathBuf;

    use runner_core::{ProviderId, Scope};

    use super::{policy, project, source_provider, tree};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{ProjectContext, Task};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        crate::tool::test_support::write_signal(&root, ProviderId::Pnpm);
        crate::tool::test_support::write_signal(&root, ProviderId::Just);
        let mut ctx = ProjectContext {
            cwd: root.clone(),
            root,
            tasks,
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        };
        crate::tool::test_support::seed_context(&mut ctx);
        ctx
    }

    fn task(name: &str, source: ProviderId) -> Task {
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
            ProviderId::PackageJson,
            ProviderId::Make,
            ProviderId::Just,
            ProviderId::Task,
            ProviderId::Turbo,
            ProviderId::Deno,
            ProviderId::Cargo,
            ProviderId::Go,
            ProviderId::Bacon,
            ProviderId::Mise,
            ProviderId::Pyproject,
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
        let ctx = context(vec![task("build", ProviderId::PackageJson)]);
        let found = project(&ctx).unwrap();
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
    fn the_download_policy_reaches_the_core_policy() {
        let overrides = ResolutionOverrides {
            download: crate::resolver::DownloadPolicy {
                value: crate::config::Download::Refuse,
                explicit: true,
            },
            ..ResolutionOverrides::default()
        };
        assert_eq!(
            policy(&overrides, Some("build")).download,
            runner_core::Download::Refuse
        );
    }

    #[test]
    fn a_task_table_chooses_its_package_manager_source_and_runtime() {
        let overrides = ResolutionOverrides::resolve(
            &crate::resolver::Invocation::default(),
            Some(&crate::config::LoadedConfig {
                path: PathBuf::from("/p/runner.toml"),
                config: toml::from_str(
                    "[tasks.build]\npm = \"pnpm\"\nsource = \
                     \"just\"\n[tasks.build.runtime]\njavascript = \"bun\"\n",
                )
                .unwrap(),
                warnings: Vec::new(),
            }),
        )
        .unwrap();
        let build = policy(&overrides, Some("build"));
        assert_eq!(
            build.pm.0.values().map(|c| c.id).collect::<Vec<_>>(),
            [ProviderId::Pnpm]
        );
        assert_eq!(build.source.map(|c| c.id), Some(ProviderId::Just));
        assert_eq!(build.runtime.map(|c| c.id), Some(ProviderId::Bun));
        let lint = policy(&overrides, Some("lint"));
        assert!(lint.pm.0.is_empty() && lint.source.is_none() && lint.runtime.is_none());
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
        std::fs::write(dir.path().join("bun.lock"), "").unwrap();
        let mut ctx = context(vec![
            task("build", ProviderId::PackageJson),
            task("build", ProviderId::Just),
        ]);
        ctx.root = dir.path().to_owned();
        ctx.cwd = ctx.root.clone();
        crate::tool::test_support::declare(&mut ctx, ProviderId::Yarn);
        let overrides = ResolutionOverrides {
            runtime: Some(RuntimeOverride {
                runtime: ProviderId::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);
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
        assert_eq!(super::task(metadata).as_ref(), Some(selected));
        assert_eq!(
            super::task_key(
                &ctx,
                &prepared.tree,
                &prepared.project,
                &prepared.policy,
                "build"
            )
            .unwrap(),
            super::super::task_output_key(metadata)
        );
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
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        let expected = std::fs::read_to_string(&path).unwrap_err().kind();
        let mut ctx = context(Vec::new());
        ctx.root = dir.path().to_owned();
        ctx.cwd = ctx.root.clone();
        crate::tool::test_support::declare(&mut ctx, ProviderId::Yarn);
        let overrides = ResolutionOverrides::default();
        crate::tool::test_support::seed_context_with(&mut ctx, &overrides);
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
