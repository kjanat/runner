//! `runner run <target>` — resolve a task name to the right tool and execute
//! it. When no task matches, fall back to executing the target as an
//! arbitrary command through the detected package manager (formerly `runner
//! exec`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::resolver::{ResolutionOverrides, Resolver};
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
    super::print_warnings(ctx);

    let (qualifier, task_name) = parse_qualified_task(task);

    let found: Vec<_> = ctx.tasks.iter().filter(|t| t.name == task_name).collect();

    if found.is_empty() {
        if let Some(code) = run_bun_test_fallback(ctx, overrides, task_name, args)? {
            return Ok(code);
        }

        if qualifier.is_none() {
            return run_pm_exec_fallback(ctx, overrides, task_name, args);
        }

        bail!("task {task:?} not found. Run `runner list` to see available tasks.");
    }

    let entry = if let Some(source) = qualifier {
        found
            .iter()
            .find(|t| t.source == source)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("task {task_name:?} not found in {}", source.label()))?
    } else {
        select_task_entry(ctx, &found)
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

pub(crate) fn select_task_entry<'a>(
    ctx: &ProjectContext,
    found: &[&'a crate::types::Task],
) -> &'a crate::types::Task {
    // Aliases rank last within any source tier so `runner <name>` dispatches
    // to the real recipe when a same-named alias exists alongside it.
    found
        .iter()
        .min_by_key(|task| {
            (
                source_priority(task.source),
                source_depth(ctx, task.source),
                task.source.display_order(),
                task.alias_of.is_some(),
            )
        })
        .copied()
        .expect("task selection should have at least one match")
}

/// Ranks sources by type before nearest-config tiebreak:
/// `TurboJson` > `PackageJson` > others. Lower is higher priority.
pub(crate) const fn source_priority(source: TaskSource) -> u8 {
    match source {
        TaskSource::TurboJson => 0,
        TaskSource::PackageJson => 1,
        _ => 2,
    }
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
/// active package manager — the resolver's choice (CLI/env/config
/// override or manifest declaration) wins over detected context.
///
/// Falls back to running the command directly when no PM is selected or
/// when the selected PM has no `exec`-like primitive (Deno, Cargo,
/// Poetry, Pipenv, Bundler, Composer, Go).
fn run_pm_exec_fallback(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
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

    // Resolver-first: honor `--pm`/`RUNNER_PM`/`runner.toml` before
    // falling back to the detected context. If none of the resolver
    // signals select a script-dispatching PM, `ctx.primary_pm()`
    // surfaces whatever lockfile-tier signal exists.
    let selected_pm = exec_pm_for_overrides(overrides).or_else(|| ctx.primary_pm());

    // Only dispatch through a PM when its exec primitive actually runs
    // arbitrary package binaries like `npx` does. For npm/yarn/pnpm/bun/uv
    // this is the whole point of `exec`. Deno, Cargo, and the Python/Ruby/
    // Go/PHP PMs have no such primitive:
    //   * Deno's `deno run <target>` treats `target` as a local script.
    //   * Cargo's `cargo <target>` dispatches to a cargo subcommand/plugin,
    //     not a binary on PATH (so `runner run eslint` in a Rust repo
    //     would try to invoke `cargo-eslint`).
    //   * Poetry/Pipenv/Bundler/Composer/Go have nothing equivalent.
    // For those we fall through to spawning `target` directly so PATH is
    // authoritative rather than silently doing the wrong thing.
    let (label, mut cmd) = match selected_pm {
        Some(PackageManager::Npm) => ("npm", tool::npm::exec_cmd(&combined())),
        Some(PackageManager::Yarn) => ("yarn", tool::yarn::exec_cmd(&combined())),
        Some(PackageManager::Pnpm) => ("pnpm", tool::pnpm::exec_cmd(&combined())),
        Some(PackageManager::Bun) => ("bun", tool::bun::exec_cmd(&combined())),
        Some(PackageManager::Uv) => ("uv", tool::uv::exec_cmd(&combined())),
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
    overrides: &ResolutionOverrides,
    task: &str,
    args: &[String],
) -> Result<Option<i32>> {
    if !should_use_bun_test_fallback(ctx, overrides, task) {
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
/// Honors overrides — `--pm npm` (or any non-Bun PM choice) suppresses
/// the Bun fallback so the user's explicit intent wins. The resolver
/// override path is what callers want anyway; the detected context is
/// only consulted when no override applies.
fn should_use_bun_test_fallback(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    task: &str,
) -> bool {
    if task != "test" || has_package_script(ctx, task) {
        return false;
    }
    let chosen = exec_pm_for_overrides(overrides)
        .or_else(|| ctx.primary_node_pm().or_else(|| ctx.primary_pm()));
    chosen.is_some_and(|pm| pm == PackageManager::Bun)
}

/// Extract a script-dispatching PM from the override bundle, in
/// resolver precedence order (CLI/env first, then per-ecosystem config).
/// Returns `None` when no override applies — caller falls back to
/// detected context.
fn exec_pm_for_overrides(overrides: &ResolutionOverrides) -> Option<PackageManager> {
    if let Some(o) = overrides.pm.as_ref()
        && pm_dispatches_scripts(o.pm)
    {
        return Some(o.pm);
    }
    overrides
        .pm_by_ecosystem
        .get(&crate::types::Ecosystem::Node)
        .or_else(|| {
            overrides
                .pm_by_ecosystem
                .get(&crate::types::Ecosystem::Deno)
        })
        .map(|o| o.pm)
}

const fn pm_dispatches_scripts(pm: PackageManager) -> bool {
    matches!(
        pm,
        PackageManager::Npm
            | PackageManager::Pnpm
            | PackageManager::Yarn
            | PackageManager::Bun
            | PackageManager::Deno
            | PackageManager::Uv
    )
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
            let decision = Resolver::new(ctx, overrides.clone()).resolve_node_pm()?;
            super::print_warning_slice(&decision.warnings);
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
    fn bun_test_fallback_enabled_when_no_test_script() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(should_use_bun_test_fallback(
            &ctx,
            &ResolutionOverrides::default(),
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
            &ResolutionOverrides::default(),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_for_other_package_managers() {
        let ctx = context(vec![PackageManager::Npm], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            &ResolutionOverrides::default(),
            "test"
        ));
    }

    #[test]
    fn bun_test_fallback_disabled_for_non_test_task() {
        let ctx = context(vec![PackageManager::Bun], vec![]);

        assert!(!should_use_bun_test_fallback(
            &ctx,
            &ResolutionOverrides::default(),
            "build"
        ));
    }

    #[test]
    fn bun_test_fallback_suppressed_by_npm_pm_override() {
        use crate::resolver::{OverrideOrigin, PmOverride};

        let ctx = context(vec![PackageManager::Bun], vec![]);
        let overrides = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Npm,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        // `--pm npm` against a Bun-detected project should not trigger the
        // Bun test fallback; user intent (npm) wins.
        assert!(!should_use_bun_test_fallback(&ctx, &overrides, "test"));
    }

    #[test]
    fn bun_test_fallback_enabled_by_bun_pm_override_without_lockfile() {
        use crate::resolver::{OverrideOrigin, PmOverride};

        // No detected PM — but the user said `--pm bun`, so the bun test
        // fallback should kick in.
        let ctx = context(vec![], vec![]);
        let overrides = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };

        assert!(should_use_bun_test_fallback(&ctx, &overrides, "test"));
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
    fn select_task_entry_prefers_nearest_deno_source() {
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
        let entry = select_task_entry(&ctx, &found);

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
}
