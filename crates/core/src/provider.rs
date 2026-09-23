//! Provider identity, ecosystem and kind.

use crate::registry::{AfterObserveFn, BeforePlanFn};

/// Index of a provider in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    /// npm.
    Npm,
    /// Yarn.
    Yarn,
    /// pnpm.
    Pnpm,
    /// Bun.
    Bun,
    /// Deno.
    Deno,
    /// Cargo.
    Cargo,
    /// Go modules.
    Go,
    /// uv.
    Uv,
    /// Poetry.
    Poetry,
    /// Pipenv.
    Pipenv,
    /// Bundler.
    Bundler,
    /// Composer.
    Composer,
    /// Turborepo.
    Turbo,
    /// Nx.
    Nx,
    /// GNU Make.
    Make,
    /// just.
    Just,
    /// go-task.
    Task,
    /// mise.
    Mise,
    /// bacon.
    Bacon,
    /// Volta.
    Volta,
    /// Node.js.
    Node,
    /// The `package.json` scripts table.
    PackageJson,
    /// The `pyproject.toml` scripts table.
    Pyproject,
}

impl ProviderId {
    /// Every id, in registry order.
    pub const ALL: [Self; 23] = [
        Self::Npm,
        Self::Yarn,
        Self::Pnpm,
        Self::Bun,
        Self::Deno,
        Self::Cargo,
        Self::Go,
        Self::Uv,
        Self::Poetry,
        Self::Pipenv,
        Self::Bundler,
        Self::Composer,
        Self::Turbo,
        Self::Nx,
        Self::Make,
        Self::Just,
        Self::Task,
        Self::Mise,
        Self::Bacon,
        Self::Volta,
        Self::Node,
        Self::PackageJson,
        Self::Pyproject,
    ];
}

/// A language the providers group under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ecosystem {
    /// Node.js.
    Node,
    /// Deno.
    Deno,
    /// Python.
    Python,
    /// Rust.
    Rust,
    /// Go.
    Go,
    /// Ruby.
    Ruby,
    /// PHP.
    Php,
    /// Task runners that belong to no language.
    Any,
}

impl Ecosystem {
    /// Every ecosystem.
    pub const ALL: [Self; 8] = [
        Self::Node,
        Self::Deno,
        Self::Python,
        Self::Rust,
        Self::Go,
        Self::Ruby,
        Self::Php,
        Self::Any,
    ];

    /// The lower-case label used in messages and config.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Deno => "deno",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Any => "any",
        }
    }
}

bitflags::bitflags! {
    /// What a provider is. A set, since deno is three of these.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Kind: u8 {
        /// Installs dependencies.
        const PACKAGE_MANAGER = 1;
        /// Declares or runs tasks.
        const TASK_SOURCE = 2;
        /// Executes source files.
        const RUNTIME = 4;
        /// Provisions other tools.
        const TOOL_MANAGER = 8;
    }
}

/// Provider code the core calls at fixed points.
#[derive(Debug, Clone, Copy)]
pub struct Hooks {
    /// A warning the core cannot know before a plan is made.
    pub before_plan: Option<BeforePlanFn>,
    /// Evidence derived from other evidence.
    pub after_observe: Option<AfterObserveFn>,
}

impl Hooks {
    /// No hooks.
    pub const NONE: Self = Self {
        before_plan: None,
        after_observe: None,
    };
}
