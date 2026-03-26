//! Shared types used across detection, commands, and tool modules.

use std::path::PathBuf;

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
    /// Poetry (Python) — detected via `poetry.lock`.
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
    /// Turborepo — detected via `turbo.json`.
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
}

/// A runnable task extracted from a project config file.
#[derive(Debug, Clone)]
pub(crate) struct Task {
    /// Name as it appears in the config (e.g. `"dev"`, `"build"`).
    pub name: String,
    /// Which config file this task was extracted from.
    pub source: TaskSource,
}

/// Identifies the config file a [`Task`] was extracted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TaskSource {
    /// `package.json` `"scripts"` field.
    PackageJson,
    /// Makefile target.
    Makefile,
    /// justfile recipe.
    Justfile,
    /// go-task `Taskfile.yml` task.
    Taskfile,
    /// `turbo.json` `"tasks"` (v2) or `"pipeline"` (v1).
    TurboJson,
    /// `deno.json` / `deno.jsonc` `"tasks"` field.
    DenoJson,
}

/// Expected Node.js version parsed from a version file.
#[derive(Debug, Clone)]
pub(crate) struct NodeVersion {
    /// The version string (e.g. `"20.11.0"`, `">=18"`).
    pub expected: String,
    /// Which file it was read from (e.g. `".nvmrc"`, `"package.json engines"`).
    pub source: &'static str,
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
        }
    }

    /// Sort priority for grouped task listings.
    pub(crate) const fn priority(self) -> u8 {
        match self {
            Self::PackageJson => 0,
            Self::Makefile => 1,
            Self::Justfile => 2,
            Self::Taskfile => 3,
            Self::TurboJson => 4,
            Self::DenoJson => 5,
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
