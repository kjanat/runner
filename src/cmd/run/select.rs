//! Source-selection logic: picking the best candidate when a task name matches multiple sources.
//!
//! The selector key is `(source_priority, source_depth, display_order,
//! is_alias)` — primary tier (Turbo > Package > others, plus prefer-list
//! offset and the forced-PM source bias), then nearest-config tiebreak,
//! then the source's canonical display order, then recipes-before-aliases.
//! Each component is pure, exposed `pub(crate)` so `cmd::why` can show the
//! same ranking key the resolver uses.
//!
//! [`source_priority`] also folds in the forced-PM source bias: a `--pm` /
//! `RUNNER_PM` override naming a package manager that is itself a distinct
//! task source (today only `deno`) pulls that source to the front of a
//! same-name conflict, so `RUNNER_PM=deno run check` picks `deno:check`
//! instead of running `package.json:check` through deno.

use std::path::{Path, PathBuf};

use crate::resolver::ResolutionOverrides;
use crate::tool;
use crate::types::{ProjectContext, TaskSource};

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
/// Layered, highest priority first:
/// - When the user forces a package manager via `--pm` / `RUNNER_PM` that is *also* a distinct
///   task source (today only `deno`, via [`crate::types::PackageManager::distinct_task_source`]),
///   that source wins its own same-name conflicts outright (priority `0`) and every other source
///   is bumped one tier down. So `RUNNER_PM=deno run check` resolves a `deno:check` /
///   `package.json:check` conflict to the native deno task instead of running the package.json
///   script *through* deno. This fires only when a PM is forced; with no `--pm` / `RUNNER_PM` the
///   ranking is unchanged.
/// - When `[task_runner].prefer = [r1, r2, ...]` is set, runners in the list win in listed order
///   (`r1 = 0`, `r2 = 1`, ...). Sources for unlisted runners fall back to the default tier offset
///   by `prefer.len()` so they always lose to listed entries.
/// - Otherwise: `TurboJson > PackageJson > others`. This is the pre-existing default and matches
///   the priority used by `runner list` for display grouping.
///
/// Lower is higher priority. Returns `u16` (rather than `u8`) to leave headroom for the offset
/// arithmetic when prefer-lists grow large without overflow on the default tier.
pub(crate) fn source_priority(overrides: &ResolutionOverrides, source: TaskSource) -> u16 {
    if let Some(forced) = forced_pm_task_source(overrides) {
        // A forced PM that owns a distinct task source wins its own
        // conflicts outright; everything else drops one tier so the
        // forced source's native task beats a same-name script in another
        // source. Single-candidate lookups are unaffected (only one task
        // to pick); only genuine conflicts re-order.
        if forced == source {
            return 0;
        }
        return base_source_priority(overrides, source).saturating_add(1);
    }
    base_source_priority(overrides, source)
}

/// The default + `[task_runner].prefer` ranking, without the forced-PM
/// source bias. Split out so [`source_priority`] can layer the bias on
/// top while keeping the unforced path byte-for-byte identical to its
/// pre-bias behavior.
fn base_source_priority(overrides: &ResolutionOverrides, source: TaskSource) -> u16 {
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

/// The task source a forced `--pm` / `RUNNER_PM` package manager owns, if
/// any. Only the cross-ecosystem CLI/env override (`overrides.pm`) counts —
/// `runner.toml` PM overrides live in `pm_by_ecosystem` and never bias
/// source selection. Today this resolves to [`TaskSource::DenoJson`] for
/// `deno` and `None` for every other PM.
fn forced_pm_task_source(overrides: &ResolutionOverrides) -> Option<TaskSource> {
    overrides
        .pm
        .as_ref()
        .and_then(|forced| forced.pm.distinct_task_source())
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
                .or_else(|| {
                    // The config lives in a subdirectory of `ctx.root`
                    // (e.g. `.cargo/config.toml` whose parent is
                    // `<root>/.cargo`). `ancestors()` only walks upward,
                    // so the position lookup never matches and depth
                    // would otherwise collapse to `usize::MAX` — making
                    // any root-level source (`bacon.toml`, `Makefile`,
                    // `justfile`) win every tiebreak by default and
                    // starving `display_order` of the chance to choose.
                    // Treat subdirectory configs as depth 0 so the
                    // tiebreak proceeds to `display_order`, which is
                    // the source-of-truth for same-tier preference.
                    dir.starts_with(&ctx.root).then_some(0)
                })
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
/// `PackageJson`, `DenoJson`, `CargoAliases`, `GoPackage`, and `Justfile` keep bespoke
/// walkers because each handles workspace boundaries (member globs in
/// `pnpm-workspace.yaml`/`deno.json`/`Cargo.toml`) or non-standard name
/// matching that the plain upward walk doesn't model.
fn source_dir(source: TaskSource, root: &Path) -> Option<PathBuf> {
    let path = match source {
        TaskSource::PackageJson => tool::node::find_manifest_upwards(root),
        TaskSource::DenoJson => tool::deno::find_config_upwards(root),
        TaskSource::CargoAliases => tool::cargo_aliases::find_anchor(root),
        TaskSource::GoPackage => tool::go_pm::find_file(root),
        TaskSource::TurboJson => tool::files::find_first_upwards(root, tool::turbo::FILENAMES),
        TaskSource::Makefile => tool::files::find_first_upwards(root, tool::make::FILENAMES),
        TaskSource::Justfile => tool::just::find_file(root),
        TaskSource::Taskfile => tool::files::find_first_upwards(root, tool::go_task::FILENAMES),
        TaskSource::BaconToml => tool::files::find_first_upwards(root, tool::bacon::FILENAMES),
        TaskSource::MiseToml => tool::files::find_first_upwards(root, tool::mise::FILENAMES),
        TaskSource::PyprojectScripts => tool::files::find_first_upwards(root, &["pyproject.toml"]),
    };
    path.and_then(|path| path.parent().map(Path::to_path_buf))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{select_task_entry, source_priority};
    use crate::resolver::{OverrideOrigin, PmOverride, ResolutionOverrides};
    use crate::types::{PackageManager, ProjectContext, Task, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers: Vec::new(),
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
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    fn pm_override(pm: PackageManager, origin: OverrideOrigin) -> ResolutionOverrides {
        ResolutionOverrides {
            pm: Some(PmOverride { pm, origin }),
            ..ResolutionOverrides::default()
        }
    }

    #[test]
    fn forced_deno_pm_wins_name_conflict_over_package_json() {
        // `RUNNER_PM=deno run check` with both a `package.json` and a
        // `deno.json` `check`: the forced PM's native task source wins so
        // runner dispatches `deno task check` instead of running the
        // package.json script through deno.
        let ctx = context(vec![
            task("check", TaskSource::PackageJson),
            task("check", TaskSource::DenoJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();

        for origin in [OverrideOrigin::CliFlag, OverrideOrigin::EnvVar] {
            let overrides = pm_override(PackageManager::Deno, origin);
            let entry = select_task_entry(&ctx, &overrides, &found);
            assert_eq!(entry.source, TaskSource::DenoJson);
        }
    }

    #[test]
    fn no_override_keeps_package_json_winning_the_conflict() {
        // `runner run check` (no PM forced): the default tier keeps
        // `package.json` ahead of `deno`, unchanged by the bias.
        let ctx = context(vec![
            task("check", TaskSource::PackageJson),
            task("check", TaskSource::DenoJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let entry = select_task_entry(&ctx, &ResolutionOverrides::default(), &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    #[test]
    fn forced_deno_beats_turbo_in_conflict() {
        // The forced-PM source bias sits above the default tier, so even
        // TurboJson (default tier 0) loses to the forced deno source.
        let ctx = context(vec![
            task("check", TaskSource::TurboJson),
            task("check", TaskSource::DenoJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = pm_override(PackageManager::Deno, OverrideOrigin::EnvVar);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::DenoJson);
    }

    #[test]
    fn forced_node_pm_without_distinct_source_does_not_reorder() {
        // Bun shares `package.json` with the other Node PMs, so it has no
        // distinct task source to bias toward: `--pm bun` leaves the
        // default ordering (package.json over deno) intact.
        let ctx = context(vec![
            task("check", TaskSource::PackageJson),
            task("check", TaskSource::DenoJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = pm_override(PackageManager::Bun, OverrideOrigin::CliFlag);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    #[test]
    fn forced_deno_priority_promotes_deno_source_to_zero() {
        // The bias is folded into `source_priority` itself so `why` and
        // `doctor`, which render that number, stay truthful: deno drops to
        // 0 and the shadowed package.json bumps one tier down.
        let overrides = pm_override(PackageManager::Deno, OverrideOrigin::CliFlag);
        assert_eq!(source_priority(&overrides, TaskSource::DenoJson), 0);
        assert!(
            source_priority(&overrides, TaskSource::PackageJson)
                > source_priority(&overrides, TaskSource::DenoJson)
        );
    }

    #[test]
    fn unforced_priority_matches_default_tier() {
        // Without a forced PM the ranking is byte-for-byte the pre-bias
        // default: Turbo (0) > package.json (1) > others (2).
        let overrides = ResolutionOverrides::default();
        assert_eq!(source_priority(&overrides, TaskSource::TurboJson), 0);
        assert_eq!(source_priority(&overrides, TaskSource::PackageJson), 1);
        assert_eq!(source_priority(&overrides, TaskSource::DenoJson), 2);
    }
}
