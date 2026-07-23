//! Source-selection logic: picking the best candidate when a task name matches multiple sources.
//!
//! The selector key is `(source_priority, source_depth, display_order,
//! is_alias)`, primary tier (Turbo > Package > others, plus prefer-list
//! offset and the forced-PM source bias), then nearest-config tiebreak,
//! then the source's canonical display order, then recipes-before-aliases.
//! Each component is pure, exposed `pub(crate)` so `cmd::why` can show the
//! same ranking key the resolver uses.
//!
//! [`source_priority`] also folds in the forced-PM source bias: a `--pm` /
//! `RUNNER_PM` override pulls the forced PM's own task source(s) to the
//! front of a same-name conflict, most-native first, so `RUNNER_PM=deno run
//! check` picks `deno:check` (a `deno task`) instead of running
//! `package.json:check` *through* deno, and `--pm bun` pulls `package.json`
//! to the front the same way. Every PM biases toward what it owns; deno is
//! one member of the rule, not a special case.

use std::path::{Path, PathBuf};

use crate::resolver::ResolutionOverrides;
use crate::tool;
use crate::types::{PackageManager, ProjectContext, TaskSource};

pub(crate) fn select_task_entry<'a>(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    found: &[&'a crate::types::Task],
) -> &'a crate::types::Task {
    // A `[tasks.overrides]` pin wins a same-name conflict, but only when the
    // user hasn't forced a PM/runner on the CLI/env, which outrank the config
    // file. The pin lists sources most-native first; the first candidate under
    // one of them wins (real recipe before a same-named alias).
    if overrides.pm.is_none()
        && overrides.runner.is_none()
        && let Some(name) = found.first().map(|t| t.name.as_str())
        && let Some(pinned) = overrides.task_source_overrides.get(name)
    {
        for source in pinned {
            if let Some(task) = found
                .iter()
                .copied()
                .filter(|t| t.source == *source)
                .min_by_key(|t| t.alias_of.is_some())
            {
                return task;
            }
        }
    }

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
/// - When the user forces a package manager via `--pm` / `RUNNER_PM`, that PM's own task
///   source(s) (via [`crate::types::PackageManager::owned_task_sources`]) win same-name conflicts,
///   most-native first: the native source takes priority `0`, the next `1`, and every other source
///   is bumped below them. So `RUNNER_PM=deno run check` resolves a `deno:check` /
///   `package.json:check` conflict to the native deno task instead of running the package.json
///   script *through* deno; `--pm bun` pulls `package.json` to the front the same way. Every PM
///   biases toward what it owns; deno is one member of the rule, not a special case. A PM that
///   owns no modeled task source (Bundler, Composer) re-orders nothing. Fires only when a PM is
///   forced; with no `--pm` / `RUNNER_PM` the ranking is unchanged.
/// - When the user forces a JS runtime via `--runtime` / `RUNNER_RUNTIME` / `[runtime].js`, the
///   sources that runtime can dispatch (`package.json`, plus `deno.json` for deno) win the same
///   way, and every other source is bumped below them. Otherwise `--runtime bun build` in a
///   turborepo picks the turbo task and forces nothing.
/// - When `[task_runner].prefer = [r1, r2, ...]` is set, runners in the list win in listed order
///   (`r1 = 0`, `r2 = 1`, ...). Sources for unlisted runners fall back to the default tier offset
///   by `prefer.len()` so they always lose to listed entries.
/// - Otherwise: `TurboJson > PackageJson > others`. This is the pre-existing default and matches
///   the priority used by `runner list` for display grouping.
///
/// Lower is higher priority. Returns `u16` (rather than `u8`) to leave headroom for the offset
/// arithmetic when prefer-lists grow large without overflow on the default tier.
pub(crate) fn source_priority(overrides: &ResolutionOverrides, source: TaskSource) -> u16 {
    // A forced runtime is checked before a forced PM: it names how the task's
    // process tree executes, which is the more specific intent, so
    // `--pm cargo --runtime bun` biases toward the sources bun can deliver
    // rather than letting the PM (which owns no JS source) drop the request.
    if let Some(runtime) = super::runtime::overridden(overrides) {
        // A forced runtime biases the same way a forced PM does, toward the
        // sources that can deliver it. Without this, `--runtime bun build` in
        // a turborepo is a guaranteed no-op: `turbo.json` outranks
        // `package.json` at the default tier and turbo selects no runtime.
        let honored = super::runtime::honored_sources(runtime);
        if let Some(idx) = honored.iter().position(|honored| *honored == source) {
            return u16::try_from(idx).unwrap_or(u16::MAX);
        }
        let bump = u16::try_from(honored.len()).unwrap_or(u16::MAX);
        return base_source_priority(overrides, source).saturating_add(bump);
    }
    if let Some(forced) = forced_pm(overrides) {
        let owned = forced.owned_task_sources();
        // The forced PM's owned task source(s) win same-name conflicts,
        // most-native first; every other source drops below them so the
        // chosen task dispatches through the PM the user asked for instead
        // of being run *through* it from a foreign source. Single-candidate
        // lookups are unaffected (only one task to pick); only genuine
        // conflicts re-order. A PM owning no source bumps nothing.
        if let Some(idx) = owned
            .iter()
            .position(|owned_source| *owned_source == source)
        {
            return u16::try_from(idx).unwrap_or(u16::MAX);
        }
        let bump = u16::try_from(owned.len()).unwrap_or(u16::MAX);
        return base_source_priority(overrides, source).saturating_add(bump);
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
    // `[tasks].prefer` (rank-only, PM-aware) is the supported preference.
    // Listed sources win in listed order; unlisted ones keep the default tier,
    // bumped below every listed entry so a listed source always wins. Never
    // restricts. Mutually exclusive with `prefer_runners` (the parser drops
    // the deprecated list when `[tasks]` is set), so the two never collide.
    if !overrides.prefer_sources.is_empty() {
        if let Some(idx) = overrides.prefer_sources.iter().position(|s| *s == source) {
            return u16::try_from(idx).unwrap_or(u16::MAX);
        }
        return u16::try_from(overrides.prefer_sources.len()).unwrap_or(u16::MAX) + default_tier;
    }
    // Deprecated `[task_runner].prefer`: restrictive elsewhere, ranked here.
    if overrides.prefer_runners.is_empty() {
        return default_tier;
    }
    if let Some(idx) = overrides
        .prefer_runners
        .iter()
        .position(|r| r.task_source() == Some(source))
    {
        // Listed runners always beat unlisted ones; the offset
        // guarantees `default_tier + prefer.len()` never collides.
        return u16::try_from(idx).unwrap_or(u16::MAX);
    }
    u16::try_from(overrides.prefer_runners.len()).unwrap_or(u16::MAX) + default_tier
}

/// The package manager forced via `--pm` / `RUNNER_PM`, if any. Only this
/// cross-ecosystem CLI/env override (`overrides.pm`) biases source
/// selection; `runner.toml` PM overrides live in `pm_by_ecosystem` and
/// never do. The caller reads its [`PackageManager::owned_task_sources`] to
/// rank the forced PM's own source(s) first.
fn forced_pm(overrides: &ResolutionOverrides) -> Option<PackageManager> {
    overrides.pm.as_ref().map(|forced| forced.pm)
}

/// Distance from `ctx.root` to the directory holding `source`'s config
/// file. Smaller values are closer; configs that don't resolve return
/// [`usize::MAX`] so they lose the tiebreak.
///
/// Generalizes the depth-aware selection that previously only fired for
/// Deno projects so that, for any pair of source candidates tied on
/// [`source_priority`], the one whose config sits in the nearest
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
                    // would otherwise collapse to `usize::MAX`, making
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{select_task_entry, source_priority};
    use crate::resolver::{OverrideOrigin, PmOverride, ResolutionOverrides, RuntimeOverride};
    use crate::types::{JsRuntime, PackageManager, ProjectContext, Task, TaskSource};

    fn context(tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
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
    fn forced_bun_biases_toward_its_own_package_json() {
        // Generalization past deno: bun owns `package.json`, so `--pm bun`
        // pulls it to the front. Here package.json already led deno, so the
        // winner is unchanged, but now it wins *because* bun biases toward
        // it, not by default tier (see the turbo case for a winner flip).
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
    fn forced_bun_beats_turbo_in_conflict() {
        // The bias is general, not deno-only: forcing a Node PM pulls its
        // `package.json` ahead of TurboJson (default tier 0) too, flipping
        // the winner from the unforced default.
        let ctx = context(vec![
            task("check", TaskSource::TurboJson),
            task("check", TaskSource::PackageJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = pm_override(PackageManager::Bun, OverrideOrigin::CliFlag);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    #[test]
    fn forced_deno_falls_back_to_package_json_when_no_deno_task() {
        // Deno owns `[deno.json, package.json]`: with no `deno.json` task to
        // pick, the same-named `package.json` script is still selected (and
        // dispatched through deno) rather than excluded.
        let ctx = context(vec![task("check", TaskSource::PackageJson)]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = pm_override(PackageManager::Deno, OverrideOrigin::EnvVar);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    #[test]
    fn forced_pm_without_owned_source_does_not_reorder() {
        // A PM that owns no modeled task source (Composer) biases nothing:
        // the package.json/deno conflict keeps its default ordering, and the
        // rendered priority is identical to the unforced path.
        let ctx = context(vec![
            task("check", TaskSource::PackageJson),
            task("check", TaskSource::DenoJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = pm_override(PackageManager::Composer, OverrideOrigin::CliFlag);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
        assert_eq!(
            source_priority(&overrides, TaskSource::PackageJson),
            source_priority(&ResolutionOverrides::default(), TaskSource::PackageJson),
        );
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
    fn forced_bun_priority_promotes_package_json_to_zero() {
        // The general rule renders truthfully for any PM: `--pm bun` drops
        // `package.json` to 0 and bumps the shadowed deno source below it.
        let overrides = pm_override(PackageManager::Bun, OverrideOrigin::CliFlag);
        assert_eq!(source_priority(&overrides, TaskSource::PackageJson), 0);
        assert!(
            source_priority(&overrides, TaskSource::DenoJson)
                > source_priority(&overrides, TaskSource::PackageJson)
        );
    }

    #[test]
    fn forced_runtime_promotes_its_source_over_turbo() {
        // `--runtime bun` in a turborepo: package.json (the source bun can
        // dispatch) drops to 0 so the flag is not a silent no-op.
        let overrides = ResolutionOverrides {
            runtime: Some(RuntimeOverride {
                runtime: JsRuntime::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };
        assert_eq!(source_priority(&overrides, TaskSource::PackageJson), 0);
        assert!(
            source_priority(&overrides, TaskSource::TurboJson)
                > source_priority(&overrides, TaskSource::PackageJson)
        );
    }

    #[test]
    fn forced_runtime_outranks_a_forced_pm_in_source_selection() {
        // Both axes set: the runtime is the more specific "run the process
        // tree on X" intent, so it wins source selection. A PM that owns no
        // JS source (cargo) would otherwise reorder nothing and let turbo
        // swallow the task, making `--runtime bun` a no-op.
        let overrides = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Cargo,
                origin: OverrideOrigin::CliFlag,
            }),
            runtime: Some(RuntimeOverride {
                runtime: JsRuntime::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            ..ResolutionOverrides::default()
        };
        assert_eq!(source_priority(&overrides, TaskSource::PackageJson), 0);
        assert!(
            source_priority(&overrides, TaskSource::TurboJson)
                > source_priority(&overrides, TaskSource::PackageJson)
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

    fn tasks_prefer(sources: Vec<TaskSource>) -> ResolutionOverrides {
        ResolutionOverrides {
            prefer_sources: sources,
            ..ResolutionOverrides::default()
        }
    }

    #[test]
    fn tasks_prefer_flips_package_json_ahead_of_turbo() {
        // The headline real-world case: `[tasks].prefer = ["bun", "turbo"]`
        // (bun → package.json) makes the package.json script win the conflict
        // that the default tier would award to turbo.
        let ctx = context(vec![
            task("build", TaskSource::TurboJson),
            task("build", TaskSource::PackageJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = tasks_prefer(vec![TaskSource::PackageJson, TaskSource::TurboJson]);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    #[test]
    fn tasks_prefer_turbo_first_keeps_turbo() {
        // The inverse order leaves turbo winning, confirms it's the listed
        // order driving the choice, not a fixed bias.
        let ctx = context(vec![
            task("build", TaskSource::TurboJson),
            task("build", TaskSource::PackageJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = tasks_prefer(vec![TaskSource::TurboJson, TaskSource::PackageJson]);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::TurboJson);
    }

    #[test]
    fn tasks_prefer_is_rank_only_unlisted_sources_still_win_when_alone() {
        // Rank-only: a task that exists *only* under an unlisted source is
        // still selected (no hard-reject, unlike the deprecated restrictive
        // `[task_runner].prefer`).
        let ctx = context(vec![task("build", TaskSource::Makefile)]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = tasks_prefer(vec![TaskSource::TurboJson]);
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::Makefile);
    }

    #[test]
    fn tasks_override_pins_a_specific_name() {
        // A per-task pin beats the global default for just that name.
        let ctx = context(vec![
            task("build", TaskSource::TurboJson),
            task("build", TaskSource::PackageJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides {
            task_source_overrides: BTreeMap::from([(
                "build".to_string(),
                vec![TaskSource::PackageJson],
            )]),
            ..ResolutionOverrides::default()
        };
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }

    #[test]
    fn tasks_override_only_affects_the_named_task() {
        // A pin for `dev` must not move the winner for `build`.
        let ctx = context(vec![
            task("build", TaskSource::TurboJson),
            task("build", TaskSource::PackageJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides {
            task_source_overrides: BTreeMap::from([(
                "dev".to_string(),
                vec![TaskSource::PackageJson],
            )]),
            ..ResolutionOverrides::default()
        };
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::TurboJson);
    }

    #[test]
    fn cli_pm_override_outranks_a_per_task_pin() {
        // Precedence: a forced `--pm`/`RUNNER_PM` (CLI/env) beats a config
        // `[tasks.overrides]` pin. Here the pin says turbo, but `--pm bun`
        // pulls package.json to the front.
        let ctx = context(vec![
            task("build", TaskSource::TurboJson),
            task("build", TaskSource::PackageJson),
        ]);
        let found: Vec<_> = ctx.tasks.iter().collect();
        let overrides = ResolutionOverrides {
            pm: Some(PmOverride {
                pm: PackageManager::Bun,
                origin: OverrideOrigin::CliFlag,
            }),
            task_source_overrides: BTreeMap::from([(
                "build".to_string(),
                vec![TaskSource::TurboJson],
            )]),
            ..ResolutionOverrides::default()
        };
        let entry = select_task_entry(&ctx, &overrides, &found);

        assert_eq!(entry.source, TaskSource::PackageJson);
    }
}
