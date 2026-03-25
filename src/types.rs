use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Bun,
    Cargo,
    Deno,
    Uv,
    Poetry,
    Pipenv,
    Go,
    Bundler,
    Composer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskRunner {
    Turbo,
    Nx,
    Make,
    Just,
    GoTask,
    Mise,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,
    pub source: TaskSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskSource {
    PackageJson,
    Makefile,
    Justfile,
    Taskfile,
    TurboJson,
    DenoJson,
}

#[derive(Debug, Clone)]
pub struct NodeVersion {
    pub expected: String,
    pub source: &'static str,
}

pub struct ProjectContext {
    pub root: PathBuf,
    pub package_managers: Vec<PackageManager>,
    pub task_runners: Vec<TaskRunner>,
    pub tasks: Vec<Task>,
    pub node_version: Option<NodeVersion>,
    pub current_node: Option<String>,
    pub is_monorepo: bool,
}

impl ProjectContext {
    pub fn primary_node_pm(&self) -> Option<PackageManager> {
        self.package_managers
            .iter()
            .copied()
            .find(|pm| pm.is_node())
    }

    pub fn primary_pm(&self) -> Option<PackageManager> {
        self.package_managers.first().copied()
    }
}

impl PackageManager {
    pub fn is_node(self) -> bool {
        matches!(self, Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun)
    }

    pub fn label(self) -> &'static str {
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
    pub fn label(self) -> &'static str {
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
    pub fn label(self) -> &'static str {
        match self {
            Self::PackageJson => "package.json",
            Self::Makefile => "Makefile",
            Self::Justfile => "justfile",
            Self::Taskfile => "Taskfile",
            Self::TurboJson => "turbo.json",
            Self::DenoJson => "deno.json",
        }
    }
}

/// Loose semver match: "20" matches "20.x.y", ">=18" matches "18+", etc.
pub fn version_matches(expected: &str, current: &str) -> bool {
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
}
