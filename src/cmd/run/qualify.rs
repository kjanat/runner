//! Task-token qualification + pre-flight validation.
//!
//! Pure structural checks on a task string (the qualifier `source:task`
//! and `member:task` syntax, reversed-qualifier detection, runner-constraint
//! matching). No subprocess spawning, no `→` arrow printed, no resolver
//! state consulted, so the chain executor can run [`precheck_task`] on
//! every item *before* any sibling dispatches.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Result, anyhow};

use super::select::{ambiguous_members, narrow_scope};
use crate::resolver::{ResolutionOverrides, ResolveError};
use crate::types::{DetectionWarning, ProjectContext, Task, TaskSource, WorkspaceMember};

/// Parse `"source:task"` syntax. Returns `(Some(source), task_name)` if the
/// prefix before the first `:` is a known source label, or `(None, original)`
/// for bare names and names with colons that don't match a source.
pub(super) fn parse_qualified_task(input: &str) -> (Option<TaskSource>, &str) {
    if let Some(colon) = input.find(':') {
        let prefix = &input[..colon];
        if let Some(source) = TaskSource::from_label(prefix) {
            return (Some(source), &input[colon + 1..]);
        }
    }
    (None, input)
}

/// Parse the `#`-separated FQN form that `doctor --json` / `why --json`
/// print as a task's identity: `<scope>:<source>#<name>`, where `scope` is
/// `root` or a workspace member name and is optional on input. Returns
/// `(scope, source, name)`, or `None` for anything whose part before the
/// `#` doesn't name a source. `user/repo#ref` package specs keep flowing to
/// the PM-exec fallback untouched.
pub(super) fn parse_fqn_task(input: &str) -> Option<(Option<&str>, TaskSource, &str)> {
    let (prefix, name) = input.split_once('#')?;
    let (scope, kind) = prefix
        .split_once(':')
        .map_or((None, prefix), |(scope, kind)| (Some(scope), kind));
    let source = TaskSource::from_label(kind)?;
    Some((scope, source, name))
}

/// The scope a token pins its lookup to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeQuery {
    /// No scope given: root tasks win, members are searched when the root
    /// has no match.
    Any,
    /// `root:` FQN prefix: root tasks only.
    Root,
    /// A workspace member named in the token.
    Member(Arc<WorkspaceMember>),
    /// A scope that named no member, or several by directory name.
    Unresolved {
        scope: String,
        candidates: Vec<String>,
    },
}

impl ScopeQuery {
    pub(crate) const fn is_pinned(&self) -> bool {
        !matches!(self, Self::Any)
    }

    fn admits(&self, task: &Task) -> bool {
        match self {
            Self::Any => true,
            Self::Root => task.member.is_none(),
            Self::Member(member) => task
                .member
                .as_ref()
                .is_some_and(|task_member| task_member.dir == member.dir),
            Self::Unresolved { .. } => false,
        }
    }
}

fn resolve_scope(ctx: &ProjectContext, scope: &str) -> ScopeQuery {
    if scope == "root" {
        return ScopeQuery::Root;
    }
    let matches = ctx
        .workspace
        .as_ref()
        .map(|workspace| workspace.resolve(scope))
        .unwrap_or_default();
    match matches.as_slice() {
        [member] => ScopeQuery::Member(Arc::clone(member)),
        [] => ScopeQuery::Unresolved {
            scope: scope.to_string(),
            candidates: Vec::new(),
        },
        several => ScopeQuery::Unresolved {
            scope: scope.to_string(),
            candidates: several.iter().map(|member| member.path.clone()).collect(),
        },
    }
}

/// A member prefix in colon syntax, when `prefix` addresses at least one
/// member. `root` is only meaningful in FQN syntax, so a bare `root:` prefix
/// stays a task name.
fn member_scope(ctx: &ProjectContext, prefix: &str) -> Option<ScopeQuery> {
    let workspace = ctx.workspace.as_ref()?;
    if prefix == "root" {
        return Some(ScopeQuery::Root);
    }
    if workspace.resolve(prefix).is_empty() {
        return None;
    }
    Some(resolve_scope(ctx, prefix))
}

struct ParsedToken<'a> {
    scope: ScopeQuery,
    source: Option<TaskSource>,
    name: &'a str,
    fqn: bool,
}

/// Interpret a token: FQN (`rfc:package.json#site`), member-qualified
/// (`rfc:site`, `rfc:package.json:site`), source-qualified (`just:fmt`), or
/// bare. A source label wins over a same-named member as the first
/// segment, matching the pre-workspace grammar.
fn parse_token<'a>(ctx: &ProjectContext, token: &'a str) -> ParsedToken<'a> {
    if let Some((scope, source, name)) = parse_fqn_task(token) {
        return ParsedToken {
            scope: scope.map_or(ScopeQuery::Any, |scope| resolve_scope(ctx, scope)),
            source: Some(source),
            name,
            fqn: true,
        };
    }
    if let Some((prefix, rest)) = token.split_once(':') {
        if let Some(source) = TaskSource::from_label(prefix) {
            return ParsedToken {
                scope: ScopeQuery::Any,
                source: Some(source),
                name: rest,
                fqn: false,
            };
        }
        if let Some(scope) = member_scope(ctx, prefix) {
            let (source, name) = parse_qualified_task(rest);
            return ParsedToken {
                scope,
                source,
                name,
                fqn: false,
            };
        }
    }
    ParsedToken {
        scope: ScopeQuery::Any,
        source: None,
        name: token,
        fqn: false,
    }
}

/// How a task token was interpreted by [`lookup_token`].
///
/// A `Some` qualifier or a pinned scope, whether from colon or FQN syntax,
/// pins the lookup; the caller errors on a miss instead of falling through
/// to PM-exec. That property is what keeps an FQN typo
/// (`root:package.json#nope`) from being handed to bun x/npx as a
/// package spec and resolved off the network.
pub(crate) struct TokenLookup<'a> {
    /// Pinned source from `source:task` or FQN (`root:source#task`) syntax.
    pub qualifier: Option<TaskSource>,
    /// Pinned scope from `member:task` or FQN (`member:source#task`) syntax.
    pub scope: ScopeQuery,
    /// Task name after stripping any qualifier.
    pub task_name: &'a str,
}

/// Interpret a task token and collect its name-matched candidates.
///
/// Single source of truth for dispatch and precheck so both agree on:
/// - FQN (`root:package.json#deno:importsmap`) → qualified lookup,
/// - colon-qualified (`deno:lint`, `rfc:site`) → qualified lookup,
/// - qualified *miss* whose raw token exactly names an existing task
///   (a `package.json` script literally called `deno:importsmap` is
///   otherwise shadowed by the `deno` source label) → bare exact match,
/// - unscoped lookups keep root tasks when the root defines the name and
///   only fall through to workspace members otherwise.
pub(crate) fn lookup_token<'a>(
    ctx: &'a ProjectContext,
    token: &'a str,
) -> (TokenLookup<'a>, Vec<&'a Task>) {
    let parsed = parse_token(ctx, token);
    let found: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|task| {
            task.name == parsed.name
                && parsed.scope.admits(task)
                && parsed.source.is_none_or(|source| task.source == source)
        })
        .collect();
    let found = if parsed.scope.is_pinned() {
        found
    } else {
        narrow_scope(ctx, found)
    };

    // Colon-form only: FQN syntax is explicit enough that a miss should
    // stay a miss rather than match a pathological `#`-bearing name.
    let qualified = parsed.source.is_some() || parsed.scope.is_pinned();
    let satisfied = found
        .iter()
        .any(|task| parsed.source.is_none_or(|source| task.source == source));
    if !parsed.fqn && qualified && !satisfied {
        let exact = narrow_scope(ctx, ctx.tasks.iter().filter(|t| t.name == token).collect());
        if !exact.is_empty() {
            return (
                TokenLookup {
                    qualifier: None,
                    scope: ScopeQuery::Any,
                    task_name: token,
                },
                exact,
            );
        }
    }

    (
        TokenLookup {
            qualifier: parsed.source,
            scope: parsed.scope,
            task_name: parsed.name,
        },
        found,
    )
}

/// The one spelling of a qualified miss, shared by dispatch and precheck
/// so `run deno:x` and `run -p deno:x` fail identically. Appends a note
/// when a source's task list failed to load; a miss caused by a broken
/// `package.json` should say so instead of leaving the user chasing the
/// task name.
pub(crate) fn qualified_miss_error(
    ctx: &ProjectContext,
    scope: &ScopeQuery,
    source: Option<TaskSource>,
    task_name: &str,
) -> anyhow::Error {
    let mut msg = match (scope, source) {
        (ScopeQuery::Unresolved { scope, candidates }, _) if !candidates.is_empty() => format!(
            "workspace member {scope:?} is ambiguous: {}. Address it by package name or path.",
            candidates.join(", "),
        ),
        (ScopeQuery::Unresolved { scope, .. }, _) => {
            let known = ctx.workspace.as_ref().map_or_else(
                || "this project declares no workspace".to_string(),
                |workspace| {
                    let names: Vec<&str> = workspace
                        .members
                        .iter()
                        .map(|member| member.label.as_str())
                        .collect();
                    if names.is_empty() {
                        "the workspace has no members".to_string()
                    } else {
                        format!("members: {}", names.join(", "))
                    }
                },
            );
            format!("no workspace member named {scope:?} ({known}).")
        }
        (ScopeQuery::Member(member), Some(source)) => format!(
            "task {task_name:?} not found in {} of workspace member {}. Run `runner list` to see \
             available tasks.",
            source.label(),
            member.name,
        ),
        (ScopeQuery::Member(member), None) => format!(
            "task {task_name:?} not found in workspace member {}. Run `runner list` to see \
             available tasks.",
            member.name,
        ),
        (_, Some(source)) => format!(
            "task {task_name:?} not found in {}. Run `runner list` to see available tasks.",
            source.label(),
        ),
        (_, None) => {
            format!("task {task_name:?} not found. Run `runner list` to see available tasks.")
        }
    };
    append_unreadable_note(ctx, &mut msg);
    anyhow!(msg)
}

/// The one spelling of a bare name that several workspace members define
/// while the root defines none, shared by dispatch, precheck, and `why`.
pub(crate) fn member_ambiguity_error(
    task_name: &str,
    members: &[&WorkspaceMember],
) -> anyhow::Error {
    let names: Vec<&str> = members.iter().map(|member| member.label.as_str()).collect();
    let spellings: Vec<String> = names
        .iter()
        .map(|name| format!("`{name}:{task_name}`"))
        .collect();
    anyhow!(
        "task {task_name:?} is defined in {} workspace members: {}\nhint: qualify it: {}",
        members.len(),
        names.join(", "),
        spellings.join(", "),
    )
}

/// The one spelling of the reversed-qualifier error (`lint:deno` instead
/// of `deno:lint`), shared by dispatch and precheck. Carries the same
/// unreadable-source note as [`qualified_miss_error`]: the ts-x509 shape
/// of this failure was a valid task name missing *because* package.json
/// didn't parse, where the `did you mean` hint alone is a red herring.
pub(super) fn reversed_qualifier_error(
    ctx: &ProjectContext,
    task: &str,
    source: TaskSource,
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
        if let DetectionWarning::TaskListUnreadable { source, .. } = warning {
            let _ = write!(
                msg,
                "\nnote: {source} failed to read, so its tasks are invisible to this lookup",
            );
        }
    }
}

/// Catch the inverted qualifier syntax (`task:source` instead of the
/// supported `source:task`). Returns `Some((source, task_name))` when
/// the *suffix* after the last `:` names a known source so the caller
/// can surface an actionable error with a `did you mean?` hint instead
/// of falling through to the PM-exec fallback and spawning a binary
/// named after the user's typo.
///
/// Matches only on the last colon so a hypothetical task name with
/// embedded colons (`foo:bar:cargo`) collapses to a single suffix check.
/// If `cargo` is the suffix, suggest `cargo:foo:bar`; otherwise let
/// the existing fallback path handle it.
pub(super) fn detect_reversed_qualifier(input: &str) -> Option<(TaskSource, &str)> {
    let colon = input.rfind(':')?;
    let suffix = &input[colon + 1..];
    let source = TaskSource::from_label(suffix)?;
    Some((source, &input[..colon]))
}

/// Side-effect-free pre-flight check for a single task token.
///
/// Catches errors the resolver would surface *without* any of the
/// expensive or stdio-visible work: no warnings emitted, no `→` arrow
/// printed, no `<pm> --version` probes. Used by chain mode to bail
/// before running *any* sibling task when a later token is clearly
/// broken, otherwise a chain like `bb t lint:cargo` runs `bb` and
/// `t` to completion before surfacing the typo at item 3.
///
/// Returns `Ok(())` on:
/// - an explicit-prefix local path (`./gen.sh`, `~/x`): dispatch runs it
///   as a file before any runner-constraint check, so precheck must too,
/// - matched task (qualified or unqualified),
/// - unmatched task whose dispatch would fall back to bun-test or
///   PM-exec: those paths require resolver state we deliberately
///   skip here; they get their proper error at dispatch time.
///
/// Returns `Err` on:
/// - qualified miss (`justfile:nonexistent`, `rfc:nonexistent`),
/// - a scope naming no member (`nope:build`) or several (`web:build`
///   with `apps/web` and `tools/web`),
/// - a bare name several members define while the root defines none,
/// - reversed qualifier (`lint:cargo`),
/// - runner-constraint mismatch (`--runner just` with no justfile task).
pub(crate) fn precheck_task(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
) -> Result<()> {
    // An explicit-prefix local path (`./gen.sh`, `~/x`, `/abs/x`) is dispatched
    // by `try_path_token` at the very top of `resolve_dispatch`, *before* the
    // runner-constraint check, so an explicit path outranks task/runner
    // resolution. Mirror that precedence here: without this escape hatch, an
    // active `--runner` / `[task_runner].prefer` constraint would make precheck
    // compute `found = []` and bail with a runner-constraint error, aborting a
    // whole chain (or install --parallel) over a token that single-run executes.
    if super::local_file::has_local_prefix(task) {
        return Ok(());
    }

    let (lookup, found) = lookup_token(ctx, task);
    let TokenLookup {
        qualifier,
        scope,
        task_name,
    } = lookup;

    if matches!(scope, ScopeQuery::Unresolved { .. }) {
        return Err(qualified_miss_error(ctx, &scope, qualifier, task_name));
    }

    let restricted: Vec<_> = if qualifier.is_some() || scope.is_pinned() {
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

    if !restricted.is_empty() {
        // For qualified inputs, also confirm the named source actually
        // produced a candidate; otherwise mirror `resolve_dispatch`'s
        // "not found in <source>" diagnostic.
        if let Some(source) = qualifier
            && !restricted.iter().any(|t| t.source == source)
        {
            return Err(qualified_miss_error(ctx, &scope, qualifier, task_name));
        }
        if !scope.is_pinned()
            && let Some(members) = ambiguous_members(&restricted)
        {
            return Err(member_ambiguity_error(task_name, &members));
        }
        return Ok(());
    }

    if qualifier.is_some() || scope.is_pinned() {
        return Err(qualified_miss_error(ctx, &scope, qualifier, task_name));
    }

    if let Some((src, task_part)) = detect_reversed_qualifier(task) {
        return Err(reversed_qualifier_error(ctx, task, src, task_part));
    }

    if let Some(reason) = runner_constraint_error(overrides, &found) {
        return Err(reason.into());
    }

    // Unqualified miss with no constraint and no reversed shape: dispatch
    // will try bun-test / PM-exec fallback. Those require resolver state
    // we intentionally skip here; defer to the dispatch-time path.
    Ok(())
}

/// Compute the set of [`TaskSource`]s the user's runner constraint
/// permits, or `None` when no constraint is active.
///
/// `--runner` / `RUNNER_RUNNER` is the strongest signal: only that
/// runner's source is allowed. `[task_runner].prefer` is the next:
/// every runner in the list is allowed, in listed order. Runners that
/// don't map to a [`TaskSource`] (`nx`, `mise`) are dropped from the
/// permission set; if that leaves the set empty under an active
/// override, [`runner_constraint_error`] surfaces the misconfiguration
/// to the user instead of silently dispatching through the default
/// priority.
pub(crate) fn allowed_runner_sources(
    overrides: &ResolutionOverrides,
) -> Option<HashSet<TaskSource>> {
    if let Some(ovr) = overrides.runner.as_ref() {
        return Some(ovr.runner.task_source().into_iter().collect());
    }
    if !overrides.prefer_runners.is_empty() {
        let set: HashSet<_> = overrides
            .prefer_runners
            .iter()
            .filter_map(|r| r.task_source())
            .collect();
        return Some(set);
    }
    None
}

/// Convert a "no candidate satisfied the runner constraint" outcome
/// into the right [`ResolveError`] for the user.
///
/// Distinguishes three failure shapes the user benefits from seeing
/// separately:
/// - `--runner nx` (a runner with no task-extraction support today) →
///   the override is unsatisfiable in principle, not just here.
/// - `--runner just` but no Justfile in this project → override is
///   set, candidates exist for the task elsewhere, but none under
///   `Justfile`.
/// - `[task_runner].prefer = [...]` with a task only under sources
///   absent from the list → analogous shape for the prefer-list.
pub(crate) fn runner_constraint_error(
    overrides: &ResolutionOverrides,
    found: &[&Task],
) -> Option<ResolveError> {
    if let Some(ovr) = overrides.runner.as_ref() {
        let label = ovr.runner.label();
        if ovr.runner.task_source().is_none() {
            return Some(ResolveError::InvalidOverride {
                value: label.to_string(),
                reason: "no task source is registered for this runner; cannot restrict candidates",
            });
        }
        let reason = if found.is_empty() {
            "no task with that name exists in the project"
        } else {
            "no candidate task is registered under this runner's source"
        };
        return Some(ResolveError::InvalidOverride {
            value: label.to_string(),
            reason,
        });
    }
    if !overrides.prefer_runners.is_empty() {
        // Stringify the list once for the user; the static `reason`
        // string can't carry the list, so the dynamic value field
        // does double duty as both the offending input and the
        // detail. Surfaced verbatim by the `Display` impl as
        // `invalid override value "[just, turbo]": ...`.
        let names = overrides
            .prefer_runners
            .iter()
            .map(|r| r.label())
            .collect::<Vec<_>>()
            .join(", ");
        return Some(ResolveError::InvalidOverride {
            value: format!("[{names}]"),
            reason: "[task_runner].prefer matched no candidate task source",
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::{ScopeQuery, lookup_token, parse_fqn_task, precheck_task};
    use crate::resolver::ResolutionOverrides;
    use crate::types::{
        DetectionWarning, ProjectContext, Task, TaskRunner, TaskSource, Workspace, WorkspaceKind,
        WorkspaceMember,
    };

    fn context() -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("."),
            root: PathBuf::from("."),
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

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            member: None,
        }
    }

    fn member(name: &str, path: &str) -> Arc<WorkspaceMember> {
        Arc::new(WorkspaceMember::new(
            name.to_string(),
            path.to_string(),
            PathBuf::from("/tmp/ws").join(path),
        ))
    }

    fn member_task(name: &str, member: &Arc<WorkspaceMember>) -> Task {
        Task {
            member: Some(Arc::clone(member)),
            ..task(name, TaskSource::PackageJson)
        }
    }

    /// Root with two members, `rfc` and `@acme/web` (at `apps/web`), both
    /// defining `site`; only `rfc` defines `check`.
    fn workspace_context() -> ProjectContext {
        let rfc = member("rfc", "rfc");
        let web = member("@acme/web", "apps/web");
        let mut ctx = context();
        ctx.workspace = Some(Workspace {
            root: PathBuf::from("/tmp/ws"),
            kinds: vec![WorkspaceKind::PackageJson],
            members: vec![Arc::clone(&rfc), Arc::clone(&web)],
            current: None,
        });
        ctx.tasks.push(task("test", TaskSource::CargoAliases));
        ctx.tasks.push(member_task("site", &rfc));
        ctx.tasks.push(member_task("check", &rfc));
        ctx.tasks.push(member_task("site", &web));
        ctx.tasks.push(member_task("test", &web));
        ctx
    }

    #[test]
    fn parse_fqn_task_accepts_doctor_fqn_forms() {
        // The exact string doctor/why print as a task's identity.
        assert_eq!(
            parse_fqn_task("root:package.json#deno:importsmap"),
            Some((Some("root"), TaskSource::PackageJson, "deno:importsmap")),
        );
        // Scope prefix optional on input.
        assert_eq!(
            parse_fqn_task("package.json#deno:importsmap"),
            Some((None, TaskSource::PackageJson, "deno:importsmap")),
        );
        assert_eq!(
            parse_fqn_task("just#fmt"),
            Some((None, TaskSource::Justfile, "fmt"))
        );
        assert_eq!(
            parse_fqn_task("rfc:package.json#site"),
            Some((Some("rfc"), TaskSource::PackageJson, "site"))
        );
    }

    #[test]
    fn parse_fqn_task_leaves_package_specs_alone() {
        // bun x/npx GitHub specs share the `#` separator; anything whose
        // prefix isn't a source label must keep flowing to PM-exec.
        assert_eq!(parse_fqn_task("user/repo#ref"), None);
        assert_eq!(parse_fqn_task("root:unknown#x"), None);
        assert_eq!(parse_fqn_task("no-hash"), None);
    }

    #[test]
    fn lookup_token_exact_name_wins_on_qualified_miss() {
        // A script literally named `deno:importsmap` was unreachable:
        // `deno` parses as a source label, so the lookup missed and dispatch
        // fell to PM-exec (ts-x509 transcript). Exact full-name match
        // must win when the qualified lookup has no candidate.
        let mut ctx = context();
        ctx.tasks
            .push(task("deno:importsmap", TaskSource::Justfile));

        let (lookup, found) = lookup_token(&ctx, "deno:importsmap");
        assert_eq!(lookup.qualifier, None);
        assert_eq!(lookup.task_name, "deno:importsmap");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn lookup_token_qualified_hit_outranks_exact_name() {
        // When deno.json really has `importsmap`, the qualified reading
        // stays authoritative even if a colon-named twin exists.
        let mut ctx = context();
        ctx.tasks.push(task("importsmap", TaskSource::DenoJson));
        ctx.tasks
            .push(task("deno:importsmap", TaskSource::PackageJson));

        let (lookup, found) = lookup_token(&ctx, "deno:importsmap");
        assert_eq!(lookup.qualifier, Some(TaskSource::DenoJson));
        assert_eq!(lookup.task_name, "importsmap");
        assert!(found.iter().any(|t| t.source == TaskSource::DenoJson));
    }

    /// [`workspace_context`] as seen from inside `rfc`, with a root `site`
    /// added so every scope defines it.
    fn inside_rfc_context() -> ProjectContext {
        let mut ctx = workspace_context();
        ctx.tasks.push(task("site", TaskSource::PackageJson));
        let workspace = ctx.workspace.as_mut().expect("workspace");
        workspace.current = workspace
            .members
            .iter()
            .find(|member| member.name == "rfc")
            .cloned();
        ctx
    }

    #[test]
    fn lookup_token_inside_a_member_prefers_that_member() {
        let ctx = inside_rfc_context();

        let (_, found) = lookup_token(&ctx, "site");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].scope(), "rfc");

        // Root still beats other members when the current member is silent.
        let (_, found) = lookup_token(&ctx, "test");
        assert_eq!(found.len(), 1);
        assert!(found[0].member.is_none());

        precheck_task(&ctx, &ResolutionOverrides::default(), "site")
            .expect("the current member resolves a name every scope defines");
    }

    #[test]
    fn lookup_token_source_qualifier_reaches_a_root_task_the_member_shadows() {
        let mut ctx = inside_rfc_context();
        ctx.tasks.push(task("site", TaskSource::Makefile));

        let (lookup, found) = lookup_token(&ctx, "make:site");
        assert_eq!(lookup.qualifier, Some(TaskSource::Makefile));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, TaskSource::Makefile);
        assert!(found[0].member.is_none());

        precheck_task(&ctx, &ResolutionOverrides::default(), "make:site")
            .expect("the source qualifier picks the scope that defines it");
    }

    #[test]
    fn lookup_token_root_prefix_reaches_a_shadowed_root_task() {
        let ctx = inside_rfc_context();

        for token in [
            "root:site",
            "root:package.json:site",
            "root:package.json#site",
        ] {
            let (lookup, found) = lookup_token(&ctx, token);
            assert_eq!(lookup.scope, ScopeQuery::Root, "{token}");
            assert_eq!(found.len(), 1, "{token}");
            assert!(found[0].member.is_none(), "{token}");
        }
    }

    #[test]
    fn lookup_token_bare_name_prefers_root_over_members() {
        let ctx = workspace_context();

        let (lookup, found) = lookup_token(&ctx, "test");
        assert_eq!(lookup.scope, ScopeQuery::Any);
        assert_eq!(found.len(), 1);
        assert!(found[0].member.is_none());
    }

    #[test]
    fn lookup_token_bare_name_falls_through_to_members() {
        let ctx = workspace_context();

        let (_, found) = lookup_token(&ctx, "check");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].scope(), "rfc");

        let (_, found) = lookup_token(&ctx, "site");
        assert_eq!(found.len(), 2, "both members surface until qualified");
    }

    #[test]
    fn lookup_token_member_prefix_pins_the_scope() {
        let ctx = workspace_context();

        for token in ["rfc:site", "rfc:package.json:site", "rfc:package.json#site"] {
            let (lookup, found) = lookup_token(&ctx, token);
            assert!(
                matches!(&lookup.scope, ScopeQuery::Member(m) if m.name == "rfc"),
                "{token}: expected the rfc scope",
            );
            assert_eq!(lookup.task_name, "site", "{token}");
            assert_eq!(found.len(), 1, "{token}");
            assert_eq!(found[0].scope(), "rfc", "{token}");
        }
    }

    #[test]
    fn lookup_token_addresses_members_by_path_and_directory_name() {
        let ctx = workspace_context();

        for token in ["@acme/web:site", "apps/web:site", "web:site"] {
            let (lookup, found) = lookup_token(&ctx, token);
            assert!(
                matches!(&lookup.scope, ScopeQuery::Member(m) if m.path == "apps/web"),
                "{token}: expected the apps/web scope",
            );
            assert_eq!(found.len(), 1, "{token}");
        }
    }

    #[test]
    fn lookup_token_root_scope_excludes_members() {
        let ctx = workspace_context();

        let (lookup, found) = lookup_token(&ctx, "root:package.json#site");
        assert_eq!(lookup.scope, ScopeQuery::Root);
        assert!(found.is_empty());
    }

    #[test]
    fn lookup_token_source_label_beats_member_name() {
        // A member literally named `just` cannot hijack `just:fmt`.
        let mut ctx = workspace_context();
        let just = member("just", "tools/just");
        ctx.workspace
            .as_mut()
            .expect("workspace")
            .members
            .push(Arc::clone(&just));
        ctx.tasks.push(member_task("fmt", &just));
        ctx.tasks.push(task("fmt", TaskSource::Justfile));

        let (lookup, found) = lookup_token(&ctx, "just:fmt");
        assert_eq!(lookup.qualifier, Some(TaskSource::Justfile));
        assert_eq!(lookup.scope, ScopeQuery::Any);
        assert!(found.iter().all(|t| t.member.is_none()));
    }

    #[test]
    fn lookup_token_unknown_member_is_reported_not_guessed() {
        let ctx = workspace_context();

        let (lookup, found) = lookup_token(&ctx, "nope:package.json#site");
        assert!(matches!(lookup.scope, ScopeQuery::Unresolved { .. }));
        assert!(found.is_empty());
    }

    #[test]
    fn lookup_token_colon_prefix_that_is_no_member_stays_a_task_name() {
        let mut ctx = workspace_context();
        ctx.tasks.push(task("fmt:update", TaskSource::PackageJson));

        let (lookup, found) = lookup_token(&ctx, "fmt:update");
        assert_eq!(lookup.scope, ScopeQuery::Any);
        assert_eq!(lookup.task_name, "fmt:update");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn precheck_rejects_ambiguous_member_task() {
        let ctx = workspace_context();

        let err = precheck_task(&ctx, &ResolutionOverrides::default(), "site")
            .expect_err("two members define site");
        let msg = format!("{err:#}");
        assert!(msg.contains("2 workspace members"), "{msg}");
        assert!(msg.contains("`rfc:site`"), "{msg}");
        assert!(msg.contains("`@acme/web:site`"), "{msg}");

        precheck_task(&ctx, &ResolutionOverrides::default(), "rfc:site")
            .expect("a qualified member task passes");
        precheck_task(&ctx, &ResolutionOverrides::default(), "check")
            .expect("a name only one member defines passes");
    }

    #[test]
    fn precheck_rejects_member_miss_and_unknown_member() {
        let ctx = workspace_context();

        let err = precheck_task(&ctx, &ResolutionOverrides::default(), "rfc:nope")
            .expect_err("rfc has no nope task");
        assert!(
            format!("{err:#}").contains("not found in workspace member rfc"),
            "{err:#}",
        );

        let err = precheck_task(
            &ctx,
            &ResolutionOverrides::default(),
            "nope:package.json#site",
        )
        .expect_err("no member is called nope");
        let msg = format!("{err:#}");
        assert!(msg.contains("no workspace member named \"nope\""), "{msg}");
        assert!(msg.contains("rfc, @acme/web"), "{msg}");
    }

    #[test]
    fn precheck_rejects_ambiguous_directory_name() {
        let mut ctx = workspace_context();
        let other = member("@acme/tool-web", "tools/web");
        ctx.workspace
            .as_mut()
            .expect("workspace")
            .members
            .push(Arc::clone(&other));
        ctx.tasks.push(member_task("site", &other));

        let err = precheck_task(&ctx, &ResolutionOverrides::default(), "web:site")
            .expect_err("two members share the directory name web");
        let msg = format!("{err:#}");
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains("apps/web"), "{msg}");
        assert!(msg.contains("tools/web"), "{msg}");

        precheck_task(&ctx, &ResolutionOverrides::default(), "tools/web:site")
            .expect("the path form is unambiguous");
    }

    #[test]
    fn precheck_passes_shadowed_colon_named_task() {
        // Chain mode (`run -p deno:importsmap …`) failed precheck with
        // `task "importsmap" not found in deno` for the same shadowing.
        let mut ctx = context();
        ctx.tasks
            .push(task("deno:importsmap", TaskSource::Justfile));

        precheck_task(&ctx, &ResolutionOverrides::default(), "deno:importsmap")
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
    fn qualified_miss_error_notes_unreadable_source() {
        // ts-x509 shape: package.json is mid-edit invalid JSON, so its
        // tasks vanish and every miss error is a red herring unless it
        // mentions the unreadable source.
        let mut ctx = context();
        ctx.warnings.push(DetectionWarning::TaskListUnreadable {
            source: "package.json",
            error: "invalid JSON".to_string(),
        });

        let err = precheck_task(&ctx, &ResolutionOverrides::default(), "deno:lint")
            .expect_err("qualified miss");
        let msg = format!("{err:#}");
        assert!(msg.contains("not found in deno"));
        assert!(msg.contains("package.json failed to read"));

        let err = precheck_task(&ctx, &ResolutionOverrides::default(), "lint:deno")
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
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };
        for token in ["./gen.sh", "../gen.sh", "/abs/gen.sh", "~/gen.sh"] {
            precheck_task(&context(), &overrides, token).unwrap_or_else(|e| {
                panic!("explicit local path {token} should precheck Ok: {e:#}")
            });
        }
    }

    #[test]
    fn precheck_rejects_prefixless_miss_under_runner_constraint() {
        // Only the explicit-prefix escape hatch is exempt: a prefix-less bare
        // name that matches no task still fails precheck under an active
        // constraint, mirroring dispatch's runner-constraint error.
        let overrides = ResolutionOverrides {
            prefer_runners: vec![TaskRunner::Just],
            ..ResolutionOverrides::default()
        };
        let err = precheck_task(&context(), &overrides, "gen")
            .expect_err("a prefix-less miss under a constraint should fail precheck");
        assert!(
            format!("{err:#}").contains("[task_runner].prefer matched no candidate"),
            "expected a runner-constraint error, got: {err:#}",
        );
    }

    #[test]
    fn precheck_does_not_restrict_under_tasks_prefer() {
        // `[tasks].prefer` is rank-only: unlike the deprecated restrictive
        // `[task_runner].prefer`, a prefix-less miss under it must NOT fail
        // precheck; nothing is hard-rejected. It only reorders.
        let overrides = ResolutionOverrides {
            prefer_sources: vec![TaskSource::TurboJson],
            ..ResolutionOverrides::default()
        };
        precheck_task(&context(), &overrides, "gen")
            .expect("[tasks].prefer must not restrict candidates");
    }
}
