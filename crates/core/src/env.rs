//! Environment layers and the variables project trust may not set.

use std::collections::BTreeMap;

use crate::provider::ProviderId;

/// Variables a project-trust config may not set. Each is a code-loading hook of its loader.
pub const LOADER_HOOKS: &[&str] = &[
    "PATH",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "RUBYOPT",
    "PERL5OPT",
    "GOFLAGS",
    "CARGO_BUILD_RUSTC",
];

/// Whether project trust may set `name`.
#[must_use]
pub fn project_may_set(name: &str) -> bool {
    !LOADER_HOOKS.contains(&name) && !name.starts_with("DYLD_")
}

/// One `KEY=value` set.
pub type EnvTable = BTreeMap<String, String>;

/// Environment layers in the order they apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvLayers {
    /// `[env]`.
    pub project: EnvTable,
    /// `[tools.<name>].env`.
    pub tool: BTreeMap<ProviderId, EnvTable>,
    /// `[tasks.<name>].env`.
    pub task: BTreeMap<String, EnvTable>,
}

#[cfg(test)]
mod tests {
    use super::project_may_set;

    #[test]
    fn loader_hooks_are_refused_at_project_trust() {
        assert!(!project_may_set("PATH"));
        assert!(!project_may_set("DYLD_INSERT_LIBRARIES"));
        assert!(project_may_set("DATABASE_URL"));
    }
}
