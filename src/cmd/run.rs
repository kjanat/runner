//! `runner run <target>` — resolve a task name to the right tool and execute
//! it. When no task matches, fall back to executing the target as an
//! arbitrary command through the detected package manager (formerly `runner
//! exec`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::resolver::{ResolutionOverrides, ResolveError, Resolver};
use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskSource};

/// Parse `"source:task"` syntax. Returns `(Some(source), task_name)` if the
/// prefix before the first `:` is a known source label, or `(None, original)`
/// for bare names and names with colons that don't match a source.
fn parse_qualified_task(input: &str) -> (Option<TaskSource>, &str) {
    if let Some(colon) = input.find(':') {
        let prefix = &input[..colon];
        if let Some(source) = TaskSource::from_label(prefix) {
            return (Some(source), &input[colon + 1..]);
        }
    }
    (None, input)
}

/// Look up `task` across all detected sources, pick the highest-priority
/// match, build the appropriate command, and execute it.
///
/// Bun special case: when `task == "test"` and no package-manifest `test`
/// script exists, falls back to `bun test`.
///
/// Returns the child process exit code.
pub(crate) fn run(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
) -> Result<i32> {
    super::print_warnings(ctx, overrides);

    let (qualifier, task_name) = parse_qualified_task(task);

    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task_name).collect();

    // `--runner X` / `[task_runner].prefer` is restrictive: when set, a
    // candidate that isn't under one of the allowed sources is treated
    // as non-existent. A qualifier (`runner.json:task`) is the user
    // narrowing *to* a source explicitly and outranks the runner
    // constraint — the qualified branch below applies its own match.
    let restricted: Vec<_> = if qualifier.is_some() {
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

    if restricted.is_empty() {
        // Restrictive override active but no candidate matched: hard
        // error per the resolved design decision (explicit intent
        // never silently downgrades).
        if let Some(reason) = runner_constraint_error(overrides, &found) {
            return Err(reason.into());
        }

        // Fallbacks are scoped to unqualified lookups. A qualified
        // miss like `runner run justfile:test` in a Bun project must
        // bail on the qualifier rather than silently dispatching
        // `bun test` — the qualifier is user intent about *where* to
        // look, not just a hint we can drop.
        //
        // The resolver call lives inside this branch so qualified
        // misses don't pay for PM resolution (warning emission,
        // potential `<pm> --version` spawn for devEngines.version
        // checks) on an error path they can't reach.
        if qualifier.is_none() {
            // Run the full resolver chain once and reuse the verdict
            // across both fallback paths. This is what closes the
            // override → config → manifest → lockfile → PATH gap that
            // the earlier no-task fallbacks had: previously, a
            // manifest-pinned pnpm/bun project with no lockfile would
            // skip the bun-test fallback (because
            // `ctx.primary_node_pm()` was empty) and direct-spawn
            // arbitrary commands through PATH instead of going
            // through the declared PM's exec primitive.
            //
            // Only the soft `NoSignalsFound { soft: true, .. }` outcome
            // (the `Probe` fallback with nothing on `$PATH`) collapses to
            // `None` so the direct PATH spawn at the bottom can still fire
            // for `runner run somebin`. Hard errors —
            // `--fallback=error`, manifest `onFail = Error`, and any
            // other resolver failure — propagate so the user sees
            // the real diagnostic instead of a silent degrade.
            let resolved_pm = match Resolver::new(ctx, overrides).resolve_node_pm() {
                Ok(decision) => {
                    super::print_warning_slice(&decision.warnings, overrides);
                    Some(decision.pm)
                }
                Err(ResolveError::NoSignalsFound { soft: true, .. }) => None,
                Err(e) => return Err(e.into()),
            };

            if let Some(code) = run_bun_test_fallback(ctx, resolved_pm, task_name, args)? {
                return Ok(code);
            }
            return run_pm_exec_fallback(ctx, resolved_pm, task_name, args);
        }

        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    let entry = if let Some(source) = qualifier {
        restricted
            .iter()
            .find(|t| t.source == source)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("task {task_name:?} not found in {}", source.label()))?
    } else {
        select_task_entry(ctx, overrides, &restricted)
    };

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        entry.source.label().dimmed(),
        task_name.bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = build_run_command(ctx, overrides, entry.source, task_name, args)?;
    super::configure_command(&mut cmd, &ctx.root);
    Ok(super::exit_code(cmd.status()?))
}

/// Compute the set of [`TaskSource`]s the user's runner constraint
/// permits, or `None` when no constraint is active.
///
/// `--runner` / `RUNNER_RUNNER` is the strongest signal — only that
/// runner's source is allowed. `[task_runner].prefer` is the next:
/// every runner in the list is allowed, in listed order. Runners that
/// don't map to a [`TaskSource`] (`nx`, `mise`) are dropped from the
/// permission set; if that leaves the set empty under an active
/// override, [`runner_constraint_error`] surfaces the misconfiguration
/// to the user instead of silently dispatching through the default
/// priority.
fn allowed_runner_sources(
    overrides: &ResolutionOverrides,
) -> Option<std::collections::HashSet<TaskSource>> {
    use std::collections::HashSet;

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
fn runner_constraint_error(
    overrides: &ResolutionOverrides,
    found: &[&crate::types::Task],
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

pub(crate) fn select_task_entry<'a>(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    found: &[&'a crate::types::Task],
) -> &'a crate::types::Task {
    // Aliases rank last within any source tier so `runner <name>` dispatches
    // to the real recipe when a same-named alias exists alongside it.
    found
        .iter()
        .min_by_key(|task| {
            (
                source_priority(overrides, task.source),
                source_depth(ctx, task.source),
                task.source.display_order(),
                task.alias_of.is_some(),
            )
        })
        .copied()
        .expect("task selection should have at least one match")
}

/// Ranks sources for the source selector's primary key.
///
/// Layered:
/// - When `[task_runner].prefer = [r1, r2, ...]` is set, runners in
///   the list win in listed order (`r1 = 0`, `r2 = 1`, ...). Sources
///   for unlisted runners fall back to the default tier offset by
///   `prefer.len()` so they always lose to listed entries.
/// - Otherwise: `TurboJson > PackageJson > others`. This is the
///   pre-existing default and matches the priority used by `runner
///   list` for display grouping.
///
/// Lower is higher priority. Returns `u16` (rather than `u8`) to leave
/// headroom for the offset arithmetic when prefer-lists grow large
/// without overflow on the default tier.
pub(crate) fn source_priority(overrides: &ResolutionOverrides, source: TaskSource) -> u16 {
    let default_tier: u16 = match source {
        TaskSource::TurboJson => 0,
        TaskSource::PackageJson => 1,
        _ => 2,
    };
    if overrides.prefer_runners.is_empty() {
        return default_tier;
    }
    if let Some(idx) = overrides
        .prefer_runners
        .iter()
        .position(|r| r.task_source() == Some(source))
    {
        // Listed runners always beat unlisted ones — the offset
        // guarantees `default_tier + prefer.len()` never collides.
        return u16::try_from(idx).unwrap_or(u16::MAX);
    }
    u16::try_from(overrides.prefer_runners.len()).unwrap_or(u16::MAX) + default_tier
}

/// Distance from `ctx.root` to the directory holding `source`'s config
/// file. Smaller values are closer; configs that don't resolve return
/// [`usize::MAX`] so they lose the tiebreak.
///
/// Generalizes the depth-aware selection that previously only fired for
/// Deno projects so that — for any pair of source candidates tied on
/// [`source_priority`] — the one whose config sits in the nearest
/// ancestor of cwd wins. Today this matters most in Deno + Node
/// workspace layouts (member `package.json` near cwd vs root
/// `deno.json`), and in Cargo + Make/Just/Taskfile setups where the
/// runner-specific file may live deeper than the workspace root.
pub(crate) fn source_depth(ctx: &ProjectContext, source: TaskSource) -> usize {
    source_dir(source, &ctx.root)
        .and_then(|dir| {
            ctx.root
                .ancestors()
                .position(|ancestor| ancestor == dir.as_path())
        })
        .unwrap_or(usize::MAX)
}

/// Locate the directory holding `source`'s config file relative to `root`.
///
/// Every source walks upward toward the repo root (stopping at the VCS
/// boundary via `tool::files::find_first_upwards`) so that
/// [`source_depth`] gives a meaningful tiebreak in nested monorepos:
/// a member `Makefile` near cwd outranks the workspace-root `Makefile`,
/// matching the precedent set by `package.json` and `deno.json`.
///
/// `PackageJson`, `DenoJson`, and `CargoAliases` keep their bespoke
/// walkers because each handles workspace boundaries (member globs in
/// `pnpm-workspace.yaml`/`deno.json`/`Cargo.toml`) that the plain
/// upward walk doesn't model.
fn source_dir(source: TaskSource, root: &Path) -> Option<PathBuf> {
    let path = match source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(root),
        TaskSource::DenoJson => tool::deno::find_config_upwards(root),
        TaskSource::CargoAliases => tool::cargo_aliases::find_anchor(root),
        TaskSource::TurboJson => tool::files::find_first_upwards(root, tool::turbo::FILENAMES),
        TaskSource::Makefile => tool::files::find_first_upwards(root, tool::make::FILENAMES),
        TaskSource::Justfile => tool::files::find_first_upwards(root, tool::just::FILENAMES),
        TaskSource::Taskfile => tool::files::find_first_upwards(root, tool::go_task::FILENAMES),
        TaskSource::BaconToml => tool::files::find_first_upwards(root, tool::bacon::FILENAMES),
    };
    path.and_then(|path| path.parent().map(Path::to_path_buf))
}

/// Execute `target` (plus `args`) as an arbitrary command through the
/// resolved package manager. `resolved_pm` carries the verdict from
/// the full resolver chain (override → manifest → lockfile → PATH
/// probe), so manifest-pinned and PATH-detected PMs participate here
/// the same way they do for `package.json` script dispatch.
///
/// Falls back to running the command directly when `resolved_pm` is
/// `None` (resolver errored under `--fallback=error`) or when the
/// selected PM has no `exec`-like primitive (Cargo, Poetry, Pipenv,
/// Bundler, Composer).
fn run_pm_exec_fallback(
    ctx: &ProjectContext,
    resolved_pm: Option<PackageManager>,
    target: &str,
    args: &[String],
) -> Result<i32> {
    // `tool::<pm>::exec_cmd` takes a flat `&[String]` — [target, ...args].
    // Build it lazily so the direct `tool::program::command(target)` fallback
    // doesn't pay for an allocation it never uses.
    let combined = || {
        let mut v = Vec::with_capacity(args.len() + 1);
        v.push(target.to_string());
        v.extend(args.iter().cloned());
        v
    };

    // Dispatch through a PM only when it owns an exec primitive that
    // can run an arbitrary target like `npx` does. Three flavors qualify:
    //   * local + registry fetch — `npx` / `npm exec`, `bun x` / `bunx`
    //   * local-only — `pnpm exec`, `yarn run` (v1) / `yarn exec` (v2+)
    //   * registry-only — `deno x` (npm:/jsr:), `uvx` (PyPI ephemeral),
    //     `go run <module>@<version>` (Go module path)
    // Cargo, Poetry, Pipenv, Bundler, and Composer have no comparable
    // primitive: `cargo <target>` would dispatch to a `cargo-<target>`
    // subcommand on PATH (so `runner run eslint` in a Rust repo would
    // look for `cargo-eslint`), and the rest are venv/Gemfile-scoped
    // launchers, not package fetchers. For those we fall through to
    // spawning `target` directly so PATH is authoritative rather than
    // silently doing the wrong thing.
    let (label, mut cmd) = match resolved_pm {
        Some(PackageManager::Npm) => ("npm", tool::npm::exec_cmd(&combined())),
        Some(PackageManager::Yarn) => ("yarn", tool::yarn::exec_cmd(&ctx.root, &combined())),
        Some(PackageManager::Pnpm) => ("pnpm", tool::pnpm::exec_cmd(&combined())),
        Some(PackageManager::Bun) => ("bun", tool::bun::exec_cmd(&combined())),
        Some(PackageManager::Deno) => ("deno x", tool::deno::exec_cmd(&combined())),
        Some(PackageManager::Uv) => ("uvx", tool::uv::exec_cmd(&combined())),
        Some(PackageManager::Go) => ("go run", tool::go_pm::exec_cmd(&combined())),
        None | Some(_) => {
            let mut c = tool::program::command(target);
            c.args(args);
            ("exec", c)
        }
    };

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        label.dimmed(),
        target.bold(),
        args.join(" ").dimmed(),
    );

    super::configure_command(&mut cmd, &ctx.root);
    Ok(super::exit_code(cmd.status()?))
}

fn run_bun_test_fallback(
    ctx: &ProjectContext,
    resolved_pm: Option<PackageManager>,
    task: &str,
    args: &[String],
) -> Result<Option<i32>> {
    if !should_use_bun_test_fallback(ctx, resolved_pm, task) {
        return Ok(None);
    }

    eprintln!(
        "{} {} {} {}",
        "→".dimmed(),
        "bun".dimmed(),
        "test".bold(),
        args.join(" ").dimmed(),
    );

    let mut cmd = tool::bun::test_cmd(args);
    super::configure_command(&mut cmd, &ctx.root);
    Ok(Some(super::exit_code(cmd.status()?)))
}

/// Bun special-case for `runner test` when the project has no
/// `package.json` `test` script: forward to `bun test`.
///
/// `resolved_pm` is the verdict from the full resolver chain, so all
/// signals — `--pm`, `RUNNER_PM`, `runner.toml`, `packageManager`,
/// `devEngines.packageManager`, lockfile, PATH probe — get a vote.
/// Fires only when the resolver landed on Bun.
fn should_use_bun_test_fallback(
    ctx: &ProjectContext,
    resolved_pm: Option<PackageManager>,
    task: &str,
) -> bool {
    if task != "test" || has_package_script(ctx, task) {
        return false;
    }
    resolved_pm.is_some_and(|pm| pm == PackageManager::Bun)
}

fn has_package_script(ctx: &ProjectContext, task: &str) -> bool {
    ctx.tasks
        .iter()
        .any(|entry| entry.source == TaskSource::PackageJson && entry.name == task)
}

/// Build a [`Command`] for the given task source and package manager.
fn build_run_command(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    source: TaskSource,
    task: &str,
    args: &[String],
) -> Result<Command> {
    Ok(match source {
        TaskSource::TurboJson => tool::turbo::run_cmd(task, args),
        TaskSource::PackageJson => {
            let decision = Resolver::new(ctx, overrides).resolve_node_pm()?;
            super::print_warning_slice(&decision.warnings, overrides);
            if overrides.explain {
                eprintln!(
                    "{} {} resolved: {}",
                    "·".dimmed(),
                    "runner".dimmed(),
                    decision.describe(),
                );
            }
            let pm = decision.pm;
            match pm {
                PackageManager::Npm => tool::npm::run_cmd(task, args),
                PackageManager::Yarn => tool::yarn::run_cmd(task, args),
                PackageManager::Pnpm => tool::pnpm::run_cmd(task, args),
                PackageManager::Bun => tool::bun::run_cmd(task, args),
                PackageManager::Deno => tool::deno::run_cmd(task, args),
                other => bail!("{} cannot run scripts", other.label()),
            }
        }
        TaskSource::Makefile => tool::make::run_cmd(task, args),
        TaskSource::Justfile => tool::just::run_cmd(task, args),
        TaskSource::Taskfile => tool::go_task::run_cmd(task, args),
        TaskSource::DenoJson => tool::deno::run_cmd(task, args),
        TaskSource::CargoAliases => tool::cargo_aliases::run_cmd(task, args),
        TaskSource::BaconToml => tool::bacon::run_cmd(task, args),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{parse_qualified_task, select_task_entry, should_use_bun_test_fallback};
    use crate::resolver::ResolutionOverrides;
    use crate::tool::test_support::TempDir;
    use crate::types::{PackageManager, ProjectContext, Task, TaskSource};

    #[test]
    fn parse_qualified_task_splits_source_and_name() {
        let (source, name) = parse_qualified_task("justfile:fmt");
        assert_eq!(source, Some(TaskSource::Justfile));
        assert_eq!(name, "fmt");
    }

    #[test]
    fn parse_qualified_task_returns_bare_name() {
        let (source, name) = parse_qualified_task("build");
        assert_eq!(source, None);
        assert_eq!(name, "build");
    }

    #[test]
    fn parse_qualified_task_handles_unknown_source() {
        let (source, name) = parse_qualified_task("unknown:build");
        assert_eq!(source, None);
        assert_eq!(name, "unknown:build");
    }

    #[test]
    fn parse_qualified_task_with_colons_in_task_name() {
        let (source, name) = parse_qualified_task("package.json:helix:sync");
        assert_eq!(source, Some(TaskSource::PackageJson));
        assert_eq!(name, "helix:sync");
    }

    #[test]
    fn parse_qualified_task_preserves_colons_in_bare_name() {
        let (source, name) = parse_qualified_task("helix:sync");
        assert_eq!(source, None);
        assert_eq!(name, "helix:sync");
    }

    #[test]
    fn parse_qualified_task_accepts_turbo_jsonc_qualifier() {
        let (source, name) = parse_qualified_task("turbo.jsonc:build");
        assert_eq!(source, Some(TaskSource::TurboJson));
        assert_eq!(name, "build");
    }

    #[test]
    fn parse_qualified_task_accepts_deno_jsonc_qualifier() {
        let (source, name) = parse_qualified_task("deno.jsonc:test");
        assert_eq!(source, Some(TaskSource::DenoJson));
        assert_eq!(name, "test");
    }

    #[test]
    fn parse_qualified_task_accepts_bacon_toml_qualifier() {
        let (source, name) = parse_qualified_task("bacon.toml:check");
        assert_eq!(source, Some(TaskSource::BaconToml));
        assert_eq!(name, "check");
    }

    #[test]
    fn bun_test_fallback_enabled_when_resolved_to_bun() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        // The resolver would return Bun via Lockfile for ctx=[Bun].
        assert!(should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_when_test_script_exists() {
        let ctx = context(
            vec![PackageManager::Bun],
            vec![Task {
                name: "test".to_string(),
                source: TaskSource::PackageJson,
                description: None,
                alias_of: None,
                passthrough_to: None,
            }],
        );

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_for_other_package_managers() {
        let ctx = context(vec![PackageManager::Npm], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Npm),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_for_non_test_task() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "build"
        ));
    }

    #[test]
    fn bun_test_fallback_suppressed_when_resolver_returns_non_bun() {
        // Models `--pm npm` against a Bun-detected project: the
        // resolver returns Npm (override wins), so the fallback must
        // not fire. The previous-shape "user intent wins" test now
        // collapses to a simpler assertion about the resolved verdict.
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Npm),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_when_resolver_returns_none() {
        // Resolver errored (--fallback=error with no signal) → no
        // fallback. Even though ctx says Bun, the caller already
        // collapsed the error to None.
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(&ctx, None, "test"));
    }

    #[test]
    fn bun_test_fallback_enabled_when_resolver_picks_bun_with_no_lockfile() {
        // Models `--pm bun` against an empty ctx — resolver returns
        // Bun even though ctx has no detected PM. Fallback fires.
        let ctx = context(vec![], vec![]);

        assert!(should_use_bun_test_fallback(
            &ctx,
            Some(PackageManager::Bun),
            "test"
        ));
    }

    #[test]
    fn source_depth_walks_upward_for_non_node_sources() {
        // Generalization landed in the same change: depth-aware tiebreak
        // used to require a custom upward walker per source. Now every
        // source consults `tool::files::find_first_upwards`, so a
        // Makefile two levels up still resolves with a finite depth (and
        // therefore beats a hypothetical sibling resolved at MAX).
        let dir = TempDir::new("source-depth-upward");
        let nested = dir.path().join("apps").join("api");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(dir.path().join("Makefile"), "build:\n\techo build\n")
            .expect("root Makefile should be written");

        let ctx = ProjectContext {
            root: nested,
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let depth = super::source_depth(&ctx, TaskSource::Makefile);
        assert_ne!(depth, usize::MAX, "Makefile two levels up should resolve");
    }

    #[test]
    fn select_task_entry_prefers_package_json_over_deno_json() {
        let dir = TempDir::new("run-deno-nearest");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ tasks: { build: "deno task build" } }"#,
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{ "scripts": { "build": "deno task build" } }"#,
        )
        .expect("member package.json should be written");
        let ctx = ProjectContext {
            root: nested,
            package_managers: vec![PackageManager::Deno],
            task_runners: Vec::new(),
            tasks: vec![
                Task {
                    name: "build".to_string(),
                    source: TaskSource::DenoJson,
                    description: None,
                    alias_of: None,
                    passthrough_to: None,
                },
                Task {
                    name: "build".to_string(),
                    source: TaskSource::PackageJson,
                    description: None,
                    alias_of: None,
                    passthrough_to: None,
                },
            ],
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        };

        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides::default();
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    fn context(package_managers: Vec<PackageManager>, tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers,
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.to_string(),
            source,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    #[test]
    fn prefer_runners_reorders_default_tier() {
        // Default priority would pick TurboJson first; `prefer = [just]`
        // promotes the Justfile candidate above it.
        let ctx = context(
            vec![],
            vec![
                task("build", TaskSource::TurboJson),
                task("build", TaskSource::Justfile),
            ],
        );
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides {
            prefer_runners: vec![crate::types::TaskRunner::Just],
            ..ResolutionOverrides::default()
        };
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::Justfile);
    }

    #[test]
    fn runner_override_promotes_just_over_turbo() {
        // `--runner just` restricts candidates; `select_task_entry` is
        // called after `run()` filters by the constraint, but with no
        // constraint helper here we exercise the priority directly.
        let ctx = context(
            vec![],
            vec![
                task("build", TaskSource::TurboJson),
                task("build", TaskSource::Justfile),
            ],
        );
        // Only the Justfile candidate survives the constraint.
        let found: Vec<&Task> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == TaskSource::Justfile)
            .collect();
        let overrides = ResolutionOverrides::default();
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::Justfile);
    }
}
