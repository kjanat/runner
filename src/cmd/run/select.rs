//! Source-selection logic: picking the best candidate when a task name matches multiple sources.
//!
//! The selector key is `(source_priority, source_depth, display_order,
//! is_alias)` — primary tier (Turbo > Package > others, plus prefer-list
//! offset), then nearest-config tiebreak, then the source's canonical
//! display order, then recipes-before-aliases. Each component is pure,
//! exposed `pub(crate)` so `cmd::why` can show the same ranking key the
//! resolver uses.

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
/// Layered:
/// - When `[task_runner].prefer = [r1, r2, ...]` is set, runners in the list win in listed order
///   (`r1 = 0`, `r2 = 1`, ...). Sources for unlisted runners fall back to the default tier offset
///   by `prefer.len()` so they always lose to listed entries.
/// - Otherwise: `TurboJson > PackageJson > others`. This is the pre-existing default and matches
///   the priority used by `runner list` for display grouping.
///
/// Lower is higher priority. Returns `u16` (rather than `u8`) to leave headroom for the offset
/// arithmetic when prefer-lists grow large without overflow on the default tier.
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
        TaskSource::MiseToml => tool::files::find_first_upwards(root, tool::mise::FILENAMES),
    };
    path.and_then(|path| path.parent().map(Path::to_path_buf))
}
