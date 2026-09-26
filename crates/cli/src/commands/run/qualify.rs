//! Pre-flight validation of a task token and the errors it reports.
//!
//! [`precheck_task`] selects through the core without spawning, probing or
//! printing, so the chain executor can check every item before any sibling
//! dispatches.

use std::fmt::Write as _;

use anyhow::{Result, anyhow};
use runner_core::Refusal;

use crate::provider::Named;
use crate::resolver::ResolutionOverrides;
use crate::types::{DetectionWarning, ProjectContext};
use runner_core::ProviderId;

/// The error for a refusal of task selection.
pub(super) fn selection_error(ctx: &ProjectContext, refusal: &Refusal) -> anyhow::Error {
    match refusal {
        Refusal::NoTask {
            name,
            source,
            scope,
        } => no_task_error(
            ctx,
            name,
            source.map(|id| runner_providers::REGISTRY.by_id(id).label),
            scope.as_deref(),
        ),
        Refusal::Ambiguous { name, candidates } => {
            let mut names: Vec<String> = Vec::new();
            for (_, scope) in candidates {
                let label = scope.label().to_owned();
                if !names.contains(&label) {
                    names.push(label);
                }
            }
            member_ambiguity_message(name, &names)
        }
        Refusal::NoRunnerTask { runner, name } => {
            let label = runner_providers::REGISTRY.by_id(*runner).label;
            anyhow!(
                "{label} defines no task named {name:?}; drop `--runner {label}` or add the task \
                 to its file"
            )
        }
        other => anyhow!("{other}"),
    }
}

/// The error for a task its token addressed by `source` or `scope` that
/// does not exist there.
fn no_task_error(
    ctx: &ProjectContext,
    name: &str,
    source: Option<&str>,
    scope: Option<&str>,
) -> anyhow::Error {
    let place = match (scope, source) {
        (Some("root"), Some(source)) => format!(" in {source} of the root"),
        (Some("root"), None) => " in the root".to_owned(),
        (Some(scope), Some(source)) => format!(" in {source} of workspace member {scope}"),
        (Some(scope), None) => format!(" in workspace member {scope}"),
        (None, Some(source)) => format!(" in {source}"),
        (None, None) => String::new(),
    };
    let mut msg =
        format!("task {name:?} not found{place}. Run `runner list` to see available tasks.");
    append_unreadable_note(ctx, &mut msg);
    anyhow!(msg)
}

/// The error for a bare name several workspace members define while the
/// root defines none.
fn member_ambiguity_message(task_name: &str, names: &[String]) -> anyhow::Error {
    let spellings: Vec<String> = names
        .iter()
        .map(|name| format!("`{name}:{task_name}`"))
        .collect();
    anyhow!(
        "task {task_name:?} is defined in {} workspace members: {}\nhint: qualify it: {}",
        names.len(),
        names.join(", "),
        spellings.join(", "),
    )
}

/// The error for a reversed qualifier (`lint:deno` instead of `deno:lint`).
fn reversed_qualifier_error(
    ctx: &ProjectContext,
    task: &str,
    source: ProviderId,
    task_part: &str,
) -> anyhow::Error {
    let src_label = source.label();
    let mut msg = format!(
        "unknown qualifier in {task:?}: source {src_label:?} must come first.\nhint: did you mean \
         \"{src_label}:{task_part}\"?",
    );
    append_unreadable_note(ctx, &mut msg);
    anyhow!(msg)
}

/// Append one `note:` line per source whose task list failed to load.
fn append_unreadable_note(ctx: &ProjectContext, msg: &mut String) {
    for warning in &ctx.warnings {
        if let DetectionWarning::Unread(unread) = warning {
            let _ = write!(
                msg,
                "\nnote: {} failed to read, so its tasks are invisible to this lookup",
                runner_providers::REGISTRY.by_id(unread.provider).label,
            );
        }
    }
}

/// The source and task a `task:source` token names when its suffix after
/// the last `:` is a source label.
pub(super) fn detect_reversed_qualifier(input: &str) -> Option<(ProviderId, &str)> {
    let colon = input.rfind(':')?;
    let suffix = &input[colon + 1..];
    let source = crate::provider::task_source(suffix)?;
    Some((source, &input[..colon]))
}

/// Check a task token without dispatching it.
///
/// Passes a local path, a builtin, a selected task, and a miss the cascade
/// may still resolve past the task rung. Refuses what selection refuses and
/// a reversed qualifier (`lint:cargo`).
///
/// # Errors
///
/// Returns observation failures and the selection refusal, rendered.
pub(crate) fn precheck_task(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
) -> Result<()> {
    if runner_core::has_local_prefix(task) || super::core::BUILTINS.contains(&task) {
        return Ok(());
    }
    let tree = super::core::tree(ctx);
    let policy = super::core::policy(overrides);
    let project = super::core::project(ctx)?;
    match super::core::selected_in(ctx, &tree, &project, &policy, task) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => match detect_reversed_qualifier(task) {
            Some((source, part)) => Err(reversed_qualifier_error(ctx, task, source, part)),
            None => Ok(()),
        },
        Err(refusal) => Err(super::dispatch::refusal_error(ctx, task, &refusal)),
    }
}

/// The task runner whose own entry point `run <token>` invokes when no
/// task carries the token's name: a detected runner with a default
/// invocation, spelled by its label, permitted by any `--runner` or
/// `[tasks].prefer` constraint.
pub(crate) fn root_runner(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
) -> Option<ProviderId> {
    let runner = crate::provider::runner(token).filter(|runner| runner.label() == token)?;
    if runner.provider().caps.run_default.is_none() || !ctx.task_runners().contains(&runner) {
        return None;
    }
    if overrides
        .runner
        .as_ref()
        .is_some_and(|ovr| ovr.runner != runner)
    {
        return None;
    }
    if !overrides.prefer_runners.is_empty() && !overrides.prefer_runners.contains(&runner) {
        return None;
    }
    Some(runner)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use runner_core::Refusal;

    use super::precheck_task;
    use crate::resolver::ResolutionOverrides;
    use crate::types::{DetectionWarning, ProjectContext, Task, Workspace, WorkspaceMember};
    use runner_core::ProviderId;

    fn context() -> ProjectContext {
        let root = crate::tool::test_support::project_root();
        ProjectContext {
            cwd: root.clone(),
            root,
            tasks: Vec::new(),
            workspace: None,
            warnings: Vec::new(),
            project: Ok(runner_core::Project::default()),
        }
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

    fn member(root: &Path, name: &str, path: &str) -> Arc<WorkspaceMember> {
        Arc::new(WorkspaceMember::new(
            name.to_string(),
            path.to_string(),
            root.join(path),
        ))
    }

    fn member_task(name: &str, member: &Arc<WorkspaceMember>) -> Task {
        Task {
            member: Some(Arc::clone(member)),
            ..task(name, ProviderId::PackageJson)
        }
    }

    fn add_member(ctx: &mut ProjectContext, name: &str, path: &str) -> Arc<WorkspaceMember> {
        let added = member(&ctx.root, name, path);
        ctx.workspace
            .as_mut()
            .expect("workspace")
            .members
            .push(Arc::clone(&added));
        added
    }

    /// Root with two members, `rfc` and `@acme/web` (at `apps/web`), both
    /// defining `site`; only `rfc` defines `check`.
    fn workspace_context() -> ProjectContext {
        let mut ctx = context();
        let rfc = member(&ctx.root, "rfc", "rfc");
        let web = member(&ctx.root, "@acme/web", "apps/web");
        ctx.workspace = Some(Workspace {
            root: ctx.root.clone(),
            kinds: vec!["package.json workspaces"],
            members: vec![Arc::clone(&rfc), Arc::clone(&web)],
            current: None,
        });
        ctx.tasks.push(task("test", ProviderId::Cargo));
        ctx.tasks.push(member_task("site", &rfc));
        ctx.tasks.push(member_task("check", &rfc));
        ctx.tasks.push(member_task("site", &web));
        ctx.tasks.push(member_task("test", &web));
        ctx
    }

    /// The task `token` selects in `ctx`, observed with its current tasks.
    fn select(ctx: &mut ProjectContext, token: &str) -> Result<Option<Task>, Refusal> {
        crate::tool::test_support::seed_context(ctx);
        let overrides = ResolutionOverrides::default();
        let tree = crate::commands::run::core::tree(ctx);
        let policy = crate::commands::run::core::policy(&overrides);
        let project = crate::commands::run::core::project(ctx).expect("observed");
        crate::commands::run::core::selected_in(ctx, &tree, &project, &policy, token)
            .map(Option::<&Task>::cloned)
    }

    fn selected(ctx: &mut ProjectContext, token: &str) -> Task {
        select(ctx, token)
            .unwrap_or_else(|refusal| panic!("{token}: {refusal}"))
            .unwrap_or_else(|| panic!("{token}: nothing selected"))
    }

    #[test]
    fn an_fqn_names_the_scope_source_and_task() {
        let mut ctx = workspace_context();
        ctx.tasks
            .push(task("deno:importsmap", ProviderId::PackageJson));
        for token in [
            "root:package.json#deno:importsmap",
            "package.json#deno:importsmap",
        ] {
            let found = selected(&mut ctx, token);
            assert_eq!(found.name, "deno:importsmap", "{token}");
            assert!(found.member.is_none(), "{token}");
        }
        assert_eq!(selected(&mut ctx, "rfc:package.json#site").scope(), "rfc");
    }

    #[test]
    fn a_package_spec_is_no_task_address() {
        let mut ctx = context();
        assert!(matches!(select(&mut ctx, "user/repo#ref"), Ok(None)));
        assert!(matches!(select(&mut ctx, "no-hash"), Ok(None)));
    }

    #[test]
    fn the_exact_name_wins_on_a_qualified_miss() {
        let mut ctx = context();
        ctx.tasks.push(task("deno:importsmap", ProviderId::Just));

        let found = selected(&mut ctx, "deno:importsmap");
        assert_eq!(found.name, "deno:importsmap");
        assert_eq!(found.source, ProviderId::Just);
    }

    #[test]
    fn a_qualified_hit_outranks_the_exact_name() {
        let mut ctx = context();
        ctx.tasks.push(task("importsmap", ProviderId::Deno));
        ctx.tasks
            .push(task("deno:importsmap", ProviderId::PackageJson));

        let found = selected(&mut ctx, "deno:importsmap");
        assert_eq!(found.source, ProviderId::Deno);
        assert_eq!(found.name, "importsmap");
    }

    /// [`workspace_context`] invoked from inside `rfc`, with a root `site`
    /// added so every scope defines it.
    fn inside_rfc_context() -> ProjectContext {
        let mut ctx = workspace_context();
        ctx.tasks.push(task("site", ProviderId::PackageJson));
        let workspace = ctx.workspace.as_mut().expect("workspace");
        let rfc = workspace
            .members
            .iter()
            .find(|member| member.name == "rfc")
            .cloned()
            .expect("rfc");
        ctx.cwd.clone_from(&rfc.dir);
        workspace.current = Some(rfc);
        ctx
    }

    #[test]
    fn inside_a_member_that_member_wins() {
        let mut ctx = inside_rfc_context();

        assert_eq!(selected(&mut ctx, "site").scope(), "rfc");
        assert!(
            selected(&mut ctx, "test").member.is_none(),
            "the root beats other members when the current one is silent"
        );
        precheck_task(&ctx, &ResolutionOverrides::default(), "site")
            .expect("the current member resolves a name every scope defines");
    }

    #[test]
    fn a_source_qualifier_reaches_a_root_task_the_member_shadows() {
        let mut ctx = inside_rfc_context();
        ctx.tasks.push(task("site", ProviderId::Make));

        let found = selected(&mut ctx, "make:site");
        assert_eq!(found.source, ProviderId::Make);
        assert!(found.member.is_none());
    }

    #[test]
    fn the_root_prefix_reaches_a_shadowed_root_task() {
        let mut ctx = inside_rfc_context();

        for token in [
            "root:site",
            "root:package.json:site",
            "root:package.json#site",
        ] {
            assert!(selected(&mut ctx, token).member.is_none(), "{token}");
        }
    }

    #[test]
    fn a_bare_name_prefers_the_root_then_falls_through_to_members() {
        let mut ctx = workspace_context();

        assert!(selected(&mut ctx, "test").member.is_none());
        assert_eq!(selected(&mut ctx, "check").scope(), "rfc");
        assert!(
            matches!(select(&mut ctx, "site"), Err(Refusal::Ambiguous { .. })),
            "both members define site"
        );
    }

    #[test]
    fn a_member_prefix_pins_the_scope() {
        let mut ctx = workspace_context();

        for token in ["rfc:site", "rfc:package.json:site", "rfc:package.json#site"] {
            let found = selected(&mut ctx, token);
            assert_eq!(found.name, "site", "{token}");
            assert_eq!(found.scope(), "rfc", "{token}");
        }
    }

    #[test]
    fn members_are_addressed_by_name_path_and_directory_name() {
        let mut ctx = workspace_context();

        for token in ["@acme/web:site", "apps/web:site", "web:site"] {
            assert_eq!(selected(&mut ctx, token).scope(), "@acme/web", "{token}");
        }
    }

    #[test]
    fn the_root_scope_excludes_members() {
        let mut ctx = workspace_context();

        assert!(matches!(
            select(&mut ctx, "root:package.json#site"),
            Err(Refusal::NoTask { .. })
        ));
    }

    #[test]
    fn a_source_label_beats_a_member_name() {
        let mut ctx = workspace_context();
        let just = add_member(&mut ctx, "just", "tools/just");
        ctx.tasks.push(member_task("fmt", &just));
        ctx.tasks.push(task("fmt", ProviderId::Just));

        let found = selected(&mut ctx, "just:fmt");
        assert_eq!(found.source, ProviderId::Just);
        assert!(found.member.is_none());
    }

    #[test]
    fn a_colon_prefix_naming_no_member_stays_part_of_the_name() {
        let mut ctx = workspace_context();
        ctx.tasks.push(task("fmt:update", ProviderId::PackageJson));

        assert_eq!(selected(&mut ctx, "fmt:update").name, "fmt:update");
    }

    /// [`precheck_task`] on `ctx` observed with its current tasks.
    fn precheck(
        ctx: &mut ProjectContext,
        overrides: &ResolutionOverrides,
        token: &str,
    ) -> anyhow::Result<()> {
        crate::tool::test_support::seed_context(ctx);
        precheck_task(ctx, overrides, token)
    }

    #[test]
    fn precheck_rejects_ambiguous_member_task() {
        let mut ctx = workspace_context();

        let err = precheck(&mut ctx, &ResolutionOverrides::default(), "site")
            .expect_err("two members define site");
        let msg = format!("{err:#}");
        assert!(msg.contains("2 workspace members"), "{msg}");
        assert!(msg.contains("`rfc:site`"), "{msg}");
        assert!(msg.contains("`@acme/web:site`"), "{msg}");

        precheck(&mut ctx, &ResolutionOverrides::default(), "rfc:site")
            .expect("a qualified member task passes");
        precheck(&mut ctx, &ResolutionOverrides::default(), "check")
            .expect("a name only one member defines passes");
    }

    #[test]
    fn precheck_lets_a_builtin_verb_through_even_when_members_share_its_name() {
        let mut ctx = workspace_context();
        let rfc = Arc::clone(&ctx.workspace.as_ref().unwrap().members[0]);
        let web = Arc::clone(&ctx.workspace.as_ref().unwrap().members[1]);
        ctx.tasks.push(member_task("list", &rfc));
        ctx.tasks.push(member_task("list", &web));

        precheck(&mut ctx, &ResolutionOverrides::default(), "list")
            .expect("the builtin rung takes `list` before any task is considered");
        precheck(&mut ctx, &ResolutionOverrides::default(), "site")
            .expect_err("a plain task two members define is still ambiguous");
    }

    #[test]
    fn precheck_rejects_member_miss_and_unknown_member() {
        let mut ctx = workspace_context();

        let err = precheck(&mut ctx, &ResolutionOverrides::default(), "rfc:nope")
            .expect_err("rfc has no nope task");
        assert!(
            format!("{err:#}").contains("not found in workspace member rfc"),
            "{err:#}",
        );

        let err = precheck(
            &mut ctx,
            &ResolutionOverrides::default(),
            "nope:package.json#site",
        )
        .expect_err("no member is called nope");
        let msg = format!("{err:#}");
        assert!(msg.contains("workspace member nope"), "{msg}");
    }

    #[test]
    fn precheck_rejects_ambiguous_directory_name() {
        let mut ctx = workspace_context();
        let other = add_member(&mut ctx, "@acme/tool-web", "tools/web");
        ctx.tasks.push(member_task("site", &other));

        let err = precheck(&mut ctx, &ResolutionOverrides::default(), "web:site")
            .expect_err("two members share the directory name web");
        let msg = format!("{err:#}");
        assert!(msg.contains("2 workspace members"), "{msg}");
        assert!(msg.contains("@acme/web"), "{msg}");
        assert!(msg.contains("@acme/tool-web"), "{msg}");

        precheck(&mut ctx, &ResolutionOverrides::default(), "tools/web:site")
            .expect("the path form is unambiguous");
    }

    #[test]
    fn precheck_passes_shadowed_colon_named_task() {
        // Chain mode (`run -p deno:importsmap …`) failed precheck with
        // `task "importsmap" not found in deno` for the same shadowing.
        let mut ctx = context();
        ctx.tasks.push(task("deno:importsmap", ProviderId::Just));

        precheck(&mut ctx, &ResolutionOverrides::default(), "deno:importsmap")
            .expect("colon-named task must pass precheck");
    }

    #[test]
    fn precheck_fqn_miss_errors_instead_of_falling_through() {
        let err = precheck_task(
            &context(),
            &ResolutionOverrides::default(),
            "root:just#nope",
        )
        .expect_err("FQN miss must fail precheck");
        assert!(format!("{err:#}").contains("not found in just"));
    }

    #[test]
    fn a_miss_notes_the_unreadable_source() {
        let mut ctx = context();
        ctx.warnings
            .push(DetectionWarning::Unread(runner_core::Unread {
                provider: ProviderId::PackageJson,
                scope: runner_core::Scope::Root,
                message: "invalid JSON".to_string(),
            }));

        let err = precheck(&mut ctx, &ResolutionOverrides::default(), "deno:lint")
            .expect_err("qualified miss");
        let msg = format!("{err:#}");
        assert!(msg.contains("not found in deno"));
        assert!(msg.contains("package.json failed to read"));

        let err = precheck(&mut ctx, &ResolutionOverrides::default(), "lint:deno")
            .expect_err("reversed qualifier");
        let msg = format!("{err:#}");
        assert!(msg.contains("deno:lint"));
        assert!(msg.contains("package.json failed to read"));
    }

    #[test]
    fn precheck_passes_explicit_local_path_under_runner_constraint() {
        // An explicit-prefix local path is dispatched as a file by
        // `try_path_token` *before* the runner-constraint check, so precheck
        // must wave it through too, otherwise a chain / install --parallel
        // under an active `[task_runner].prefer` aborts on a token that a
        // single `run ./gen.sh` executes fine.
        let overrides = ResolutionOverrides {
            prefer_runners: vec![ProviderId::Just],
            ..ResolutionOverrides::default()
        };
        for token in ["./gen.sh", "../gen.sh", "/abs/gen.sh", "~/gen.sh"] {
            precheck_task(&context(), &overrides, token).unwrap_or_else(|e| {
                panic!("explicit local path {token} should precheck Ok: {e:#}")
            });
        }
    }

    #[test]
    fn precheck_passes_root_invocation_under_matching_runner_constraint() {
        let mut ctx = context();
        crate::tool::test_support::declare(&mut ctx, ProviderId::Make);
        let overrides = ResolutionOverrides {
            runner: Some(crate::resolver::RunnerOverride {
                runner: ProviderId::Make,
                origin: crate::resolver::OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        precheck(&mut ctx, &overrides, "make").expect("`make` invokes make's own entry point");
    }

    #[test]
    fn precheck_passes_root_invocation_under_a_prefer_list_without_make() {
        let mut ctx = context();
        crate::tool::test_support::declare(&mut ctx, ProviderId::Make);
        let preferred = ResolutionOverrides {
            prefer_runners: vec![ProviderId::Just],
            ..ResolutionOverrides::default()
        };

        precheck(&mut ctx, &preferred, "make")
            .expect("a prefer list ranks and never restricts the host rung");
    }

    #[test]
    fn precheck_passes_a_bare_miss_under_a_runner_choice_to_the_cascade() {
        let overrides = ResolutionOverrides {
            prefer_runners: vec![ProviderId::Just],
            ..ResolutionOverrides::default()
        };
        precheck_task(&context(), &overrides, "gen")
            .expect("a runner choice ranks task candidates and refuses nothing");
    }

    #[test]
    fn precheck_does_not_restrict_under_tasks_prefer() {
        // `[tasks].prefer` is rank-only: unlike the deprecated restrictive
        // `[task_runner].prefer`, a prefix-less miss under it must NOT fail
        // precheck; nothing is hard-rejected. It only reorders.
        let overrides = ResolutionOverrides {
            prefer_sources: vec![ProviderId::Turbo],
            ..ResolutionOverrides::default()
        };
        precheck_task(&context(), &overrides, "gen")
            .expect("[tasks].prefer must not restrict candidates");
    }
}
