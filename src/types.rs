//! Shared types used across detection, commands, and tool modules.

use std::path::PathBuf;

/// A language/runtime ecosystem that owns one or more package managers.
///
/// Used by the resolver to scope overrides — a `[pm].node = "pnpm"` entry
/// in `runner.toml` applies only when resolving for [`Ecosystem::Node`].
/// Deno is its own ecosystem even though its package manager can also
/// dispatch `package.json` scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Ecosystem {
    /// Node.js (npm, yarn, pnpm, bun).
    Node,
    /// Deno.
    Deno,
    /// Python (uv, poetry, pipenv).
    Python,
    /// Rust (cargo).
    Rust,
    /// Go.
    Go,
    /// Ruby (bundler).
    Ruby,
    /// PHP (composer).
    Php,
}

/// A dependency manager detected via lockfile or config presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PackageManager {
    /// npm — detected via `package-lock.json`.
    Npm,
    /// Yarn — detected via `yarn.lock`.
    Yarn,
    /// pnpm — detected via `pnpm-lock.yaml`.
    Pnpm,
    /// Bun — detected via `bun.lockb` or `bun.lock`.
    Bun,
    /// Cargo (Rust) — detected via `Cargo.toml`.
    Cargo,
    /// Deno — detected via `deno.json` / `deno.jsonc`.
    Deno,
    /// uv (Python) — detected via `uv.lock`.
    Uv,
    /// Poetry (Python) — detected via `poetry.lock` or Poetry `pyproject.toml` markers.
    Poetry,
    /// Pipenv (Python) — detected via `Pipfile` / `Pipfile.lock`.
    Pipenv,
    /// Go modules — detected via `go.mod`.
    Go,
    /// Bundler (Ruby) — detected via `Gemfile`.
    Bundler,
    /// Composer (PHP) — detected via `composer.json`.
    Composer,
}

/// A task runner detected via config file presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TaskRunner {
    /// Turborepo — detected via `turbo.json` / `turbo.jsonc`.
    Turbo,
    /// Nx — detected via `nx.json`.
    Nx,
    /// GNU Make — detected via `Makefile` / `GNUmakefile` / `makefile`.
    Make,
    /// just — detected via `justfile` / `Justfile` / `.justfile`.
    Just,
    /// go-task — detected via `Taskfile.yml` and variants.
    GoTask,
    /// mise — detected via `mise.toml` / `.mise.toml`.
    Mise,
    /// bacon — detected via `bacon.toml`.
    Bacon,
}

/// A runnable task extracted from a project config file.
#[derive(Debug, Clone)]
pub(crate) struct Task {
    /// Name as it appears in the config (e.g. `"dev"`, `"build"`).
    pub name: String,
    /// Which config file this task was extracted from.
    pub source: TaskSource,
    /// Optional human-readable description (e.g. justfile doc comment,
    /// go-task `desc` field).
    pub description: Option<String>,
    /// When this task is an alias, the name of the target recipe it
    /// resolves to (e.g. `alias b := build` → `Some("build")`).
    pub alias_of: Option<String>,
    /// `Some(runner)` when this task's command body is a thin
    /// passthrough to a task runner for a same-named target — e.g. a
    /// `package.json` script `"build": "just build"` records
    /// `Some(TaskRunner::Just)`. Set during detection by inspecting the
    /// actual script body, not inferred from name collisions, so real
    /// scripts like `"build": "vite build"` are never flagged. Used by
    /// completion to avoid emitting a redundant `package.json:build`
    /// candidate alongside the underlying runner's `build` task.
    pub passthrough_to: Option<TaskRunner>,
}

/// Identifies the config file a [`Task`] was extracted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TaskSource {
    /// Node package manifest `"scripts"` field (`package.json`,
    /// `package.json5`, `package.yaml`).
    PackageJson,
    /// Makefile target.
    Makefile,
    /// justfile recipe.
    Justfile,
    /// go-task `Taskfile.yml` task.
    Taskfile,
    /// `turbo.json` / `turbo.jsonc` `"tasks"` (v2) or `"pipeline"` (v1).
    TurboJson,
    /// `deno.json` / `deno.jsonc` `"tasks"` field.
    DenoJson,
    /// Cargo `[alias]` table — built-ins plus user aliases merged across the
    /// hierarchical `.cargo/config.toml` chain.
    CargoAliases,
    /// `bacon.toml` `[jobs.<name>]` tables.
    BaconToml,
}

/// Expected Node.js version parsed from a version file.
#[derive(Debug, Clone)]
pub(crate) struct NodeVersion {
    /// The version string (e.g. `"20.11.0"`, `">=18"`).
    pub expected: String,
    /// Which file it was read from (e.g. `".nvmrc"`, `"package.json engines"`).
    pub source: &'static str,
}

/// Non-fatal issue found while detecting project metadata.
#[derive(Debug, Clone)]
pub(crate) struct DetectionWarning {
    /// Which config or subsystem produced the warning.
    pub source: &'static str,
    /// Human-readable detail for the user.
    pub detail: String,
}

/// Everything detected about the current project directory.
pub(crate) struct ProjectContext {
    /// Absolute path to the project root that was scanned.
    pub root: PathBuf,
    /// Detected package managers, ordered by detection priority.
    pub package_managers: Vec<PackageManager>,
    /// Detected task runners.
    pub task_runners: Vec<TaskRunner>,
    /// All extracted tasks, sorted by source then name.
    pub tasks: Vec<Task>,
    /// Expected Node.js version from `.nvmrc`, `.node-version`, etc.
    pub node_version: Option<NodeVersion>,
    /// Currently installed Node.js version (from `node --version`).
    pub current_node: Option<String>,
    /// Whether the project appears to be a monorepo.
    pub is_monorepo: bool,
    /// Non-fatal detection issues surfaced to task-facing commands.
    pub warnings: Vec<DetectionWarning>,
}

impl ProjectContext {
    /// Returns the first Node-ecosystem package manager, if any.
    pub(crate) fn primary_node_pm(&self) -> Option<PackageManager> {
        self.package_managers
            .iter()
            .copied()
            .find(|pm| pm.is_node())
    }

    /// Returns the first detected package manager of any ecosystem.
    pub(crate) fn primary_pm(&self) -> Option<PackageManager> {
        self.package_managers.first().copied()
    }
}

impl PackageManager {
    /// Returns `true` for Node.js package managers (npm, yarn, pnpm, bun).
    pub(crate) const fn is_node(self) -> bool {
        matches!(self, Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun)
    }

    /// Human-readable CLI name (e.g. `"pnpm"`, `"cargo"`).
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
            Self::Bun => "bun",
            Self::Cargo => "cargo",
            Self::Deno => "deno",
            Self::Uv => "uv",
            Self::Poetry => "poetry",
            Self::Pipenv => "pipenv",
            Self::Go => "go",
            Self::Bundler => "bundler",
            Self::Composer => "composer",
        }
    }

    /// Parse a user-supplied label (CLI flag value, env var, config field)
    /// back into a [`PackageManager`].
    ///
    /// Accepts the canonical `label()` for each variant and the common
    /// `bundle` alias for Ruby's Bundler (which spells its binary `bundle`).
    /// Surrounding whitespace is trimmed so `" pnpm "` from a padded env
    /// var or TOML value still parses; resolver-side parsing also trims
    /// before this is reached but config-loader call sites pass raw
    /// strings.
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label.trim() {
            "npm" => Some(Self::Npm),
            "yarn" => Some(Self::Yarn),
            "pnpm" => Some(Self::Pnpm),
            "bun" => Some(Self::Bun),
            "cargo" => Some(Self::Cargo),
            "deno" => Some(Self::Deno),
            "uv" => Some(Self::Uv),
            "poetry" => Some(Self::Poetry),
            "pipenv" => Some(Self::Pipenv),
            "go" => Some(Self::Go),
            "bundler" | "bundle" => Some(Self::Bundler),
            "composer" => Some(Self::Composer),
            _ => None,
        }
    }

    /// Every variant of [`PackageManager`] in a fixed order, for help text
    /// and error messages that need to enumerate the valid values.
    pub(crate) const fn all() -> &'static [Self] {
        &[
            Self::Npm,
            Self::Yarn,
            Self::Pnpm,
            Self::Bun,
            Self::Cargo,
            Self::Deno,
            Self::Uv,
            Self::Poetry,
            Self::Pipenv,
            Self::Go,
            Self::Bundler,
            Self::Composer,
        ]
    }

    /// The ecosystem this package manager belongs to.
    pub(crate) const fn ecosystem(self) -> Ecosystem {
        match self {
            Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun => Ecosystem::Node,
            Self::Deno => Ecosystem::Deno,
            Self::Cargo => Ecosystem::Rust,
            Self::Uv | Self::Poetry | Self::Pipenv => Ecosystem::Python,
            Self::Go => Ecosystem::Go,
            Self::Bundler => Ecosystem::Ruby,
            Self::Composer => Ecosystem::Php,
        }
    }

    /// Whether this PM can dispatch a script declared in `package.json`
    /// `"scripts"` — Node ecosystem (`npm`, `yarn`, `pnpm`, `bun`) plus
    /// Deno (via `deno run <task>`). Used by both the resolver (to
    /// scope `--pm` overrides for Node-script resolution) and the
    /// bun-test fallback path (to answer "did the user pick a
    /// Node-script PM other than Bun?").
    pub(crate) const fn can_dispatch_node_scripts(self) -> bool {
        self.is_node() || matches!(self, Self::Deno)
    }

    /// Whether this PM owns an exec primitive that resolves a target
    /// to runnable code: `npm exec` / `npx` (local + registry fetch),
    /// `pnpm exec` (local only), `yarn exec` / `yarn run` (local
    /// only, version-aware shape), `bun x` / `bunx` (local + registry
    /// fetch), `deno x` (`npm:` / `jsr:` registry fetch), `uvx` (`PyPI`
    /// ephemeral), `go run <path>@version` (module-path registry
    /// fetch). The arbitrary-command fallback in
    /// `cmd::run::run_pm_exec_fallback` dispatches through these via
    /// per-PM `exec_cmd` builders; the remaining PMs
    /// (Cargo, Poetry, Pipenv, Bundler, Composer) lack a comparable
    /// primitive in this implementation and fall through to a direct
    /// PATH spawn there. Kept as an inherent method so the property
    /// is documented at the type level even though the dispatch match
    /// enumerates variants explicitly.
    #[allow(
        dead_code,
        reason = "documents the exec-primitive set; consumed by doctor/why surface in future enhancements"
    )]
    pub(crate) const fn has_exec_primitive(self) -> bool {
        matches!(
            self,
            Self::Npm | Self::Pnpm | Self::Yarn | Self::Bun | Self::Deno | Self::Uv | Self::Go
        )
    }
}

impl TaskRunner {
    /// Human-readable CLI name (e.g. `"turbo"`, `"just"`).
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Turbo => "turbo",
            Self::Nx => "nx",
            Self::Make => "make",
            Self::Just => "just",
            Self::GoTask => "task",
            Self::Mise => "mise",
            Self::Bacon => "bacon",
        }
    }

    /// Parse a user-supplied label (CLI flag value, env var, config field)
    /// back into a [`TaskRunner`].
    ///
    /// Accepts the canonical `label()` plus the alias `go-task` for `task`
    /// to disambiguate from arbitrary task names. Surrounding whitespace is
    /// trimmed to match [`PackageManager::from_label`].
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label.trim() {
            "turbo" => Some(Self::Turbo),
            "nx" => Some(Self::Nx),
            "make" => Some(Self::Make),
            "just" => Some(Self::Just),
            "task" | "go-task" => Some(Self::GoTask),
            "mise" => Some(Self::Mise),
            "bacon" => Some(Self::Bacon),
            _ => None,
        }
    }

    /// Every variant of [`TaskRunner`] in a fixed order, for help text and
    /// error messages that need to enumerate the valid values.
    pub(crate) const fn all() -> &'static [Self] {
        &[
            Self::Turbo,
            Self::Nx,
            Self::Make,
            Self::Just,
            Self::GoTask,
            Self::Mise,
            Self::Bacon,
        ]
    }

    /// The [`TaskSource`] that holds this runner's tasks, when extraction
    /// is implemented for it. Used by completion to dedupe `package.json`
    /// passthrough wrappers against the underlying runner's task entry.
    ///
    /// Returns `None` for runners where task extraction is not yet
    /// implemented (Nx, Mise); a passthrough wrapper still routes to that
    /// runner at dispatch time, but completion shows the script as its
    /// own candidate because there is no peer entry to collapse it into.
    pub(crate) const fn task_source(self) -> Option<TaskSource> {
        match self {
            Self::Turbo => Some(TaskSource::TurboJson),
            Self::Make => Some(TaskSource::Makefile),
            Self::Just => Some(TaskSource::Justfile),
            Self::GoTask => Some(TaskSource::Taskfile),
            Self::Bacon => Some(TaskSource::BaconToml),
            Self::Nx | Self::Mise => None,
        }
    }
}

impl TaskSource {
    /// Config filename shown to the user (e.g. `"package.json"`, `"Makefile"`).
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::PackageJson => "package.json",
            Self::Makefile => "Makefile",
            Self::Justfile => "justfile",
            Self::Taskfile => "Taskfile",
            Self::TurboJson => "turbo.json",
            Self::DenoJson => "deno.json",
            // Synthetic source — aliases merge across the hierarchical
            // `.cargo/config.toml` chain plus `$CARGO_HOME`, so no single
            // file name represents it.
            Self::CargoAliases => "cargo",
            Self::BaconToml => "bacon.toml",
        }
    }

    /// Parse a source label back to a [`TaskSource`].
    pub(crate) fn from_label(label: &str) -> Option<Self> {
        match label {
            "package.json" => Some(Self::PackageJson),
            "Makefile" => Some(Self::Makefile),
            "justfile" => Some(Self::Justfile),
            "Taskfile" => Some(Self::Taskfile),
            "turbo.json" | "turbo.jsonc" => Some(Self::TurboJson),
            "deno.json" | "deno.jsonc" => Some(Self::DenoJson),
            "cargo" => Some(Self::CargoAliases),
            "bacon.toml" => Some(Self::BaconToml),
            _ => None,
        }
    }

    /// Display order for grouped task listings.
    pub(crate) const fn display_order(self) -> u8 {
        match self {
            Self::PackageJson => 0,
            Self::Makefile => 1,
            Self::Justfile => 2,
            Self::Taskfile => 3,
            Self::TurboJson => 4,
            Self::DenoJson => 5,
            Self::CargoAliases => 6,
            Self::BaconToml => 7,
        }
    }
}

/// Loose semver prefix match.
///
/// Strips leading range operators (`>=`, `~`, `^`, etc.) and checks whether
/// `current` starts with the cleaned `expected` value. A bare major version
/// like `"20"` matches `"20.x.y"`.
///
/// This intentionally ignores operator semantics — use the `semver` crate
/// for precise constraint evaluation.
pub(crate) fn version_matches(expected: &str, current: &str) -> bool {
    let expected = expected.trim();
    let current = current.trim();

    let expected_clean = expected
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('~')
        .trim_start_matches('^')
        .trim_start_matches('v')
        .trim();

    if !expected_clean.contains('.') {
        return current.starts_with(expected_clean)
            && current[expected_clean.len()..]
                .chars()
                .next()
                .is_none_or(|c| c == '.');
    }

    current.starts_with(expected_clean)
        && current[expected_clean.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == '.')
}

#[cfg(test)]
mod tests {
    use super::version_matches;

    #[test]
    fn dotted_versions_match_segment_boundaries_only() {
        assert!(version_matches("20.11", "20.11.0"));
        assert!(!version_matches("20.11", "20.110.0"));
    }
}
