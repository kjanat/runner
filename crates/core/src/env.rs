//! Environment layers and the variables project trust may not set.

use std::collections::BTreeMap;

use crate::provider::ProviderId;

/// Variables a project-trust config may not set. Each is a code-loading hook of its loader.
pub const LOADER_HOOKS: &[&str] = &[
    "PATH",
    "BASH_ENV",
    "ENV",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "NODE_OPTIONS",
    "BUN_OPTIONS",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONUSERBASE",
    "RUBYOPT",
    "RUBYLIB",
    "PERL5OPT",
    "PERL5LIB",
    "PERLLIB",
    "PERL5DB",
    "GOFLAGS",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_TEMPLATE_DIR",
    "MAKEFILES",
    "YARN_RC_FILENAME",
    "YARN_YARN_PATH",
    "GCONV_PATH",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
];

const LOADER_HOOK_PREFIXES: &[&str] = &["DYLD_", "GIT_CONFIG", "BASH_FUNC_"];

const CARGO_TARGET_HOOK_SUFFIXES: &[&str] = &["_RUNNER", "_LINKER"];

const NPM_CONFIG_HOOKS: &[&str] = &["node-options", "script-shell", "userconfig", "globalconfig"];

/// Whether project trust may set `name`.
#[must_use]
pub fn project_may_set(name: &str) -> bool {
    #[cfg(windows)]
    let name = name.to_ascii_uppercase();
    #[cfg(windows)]
    let name = name.as_str();
    !LOADER_HOOKS.contains(&name)
        && !LOADER_HOOK_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        && !is_cargo_target_hook(name)
        && !is_npm_config_hook(name)
}

fn is_cargo_target_hook(name: &str) -> bool {
    name.strip_prefix("CARGO_TARGET_").is_some_and(|rest| {
        CARGO_TARGET_HOOK_SUFFIXES
            .iter()
            .any(|suffix| rest.ends_with(suffix))
    })
}

fn is_npm_config_hook(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.strip_prefix("npm_config_")
        .is_some_and(|key| NPM_CONFIG_HOOKS.contains(&key.replace('_', "-").as_str()))
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
        for name in [
            "PATH",
            "DYLD_INSERT_LIBRARIES",
            "BASH_ENV",
            "PYTHONPATH",
            "RUBYLIB",
            "PERL5LIB",
            "RUSTC_WRAPPER",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "npm_config_node_options",
            "NPM_CONFIG_SCRIPT_SHELL",
            "npm_config_script-shell",
            "BASH_FUNC_echo%%",
            "MAKEFILES",
            "NPM_CONFIG_USERCONFIG",
            "npm_config_userconfig",
            "npm_config_globalconfig",
            "Npm_Config_GlobalConfig",
            "YARN_RC_FILENAME",
            "YARN_YARN_PATH",
            "GIT_ASKPASS",
            "SSH_ASKPASS",
            "GIT_PROXY_COMMAND",
            "GIT_TEMPLATE_DIR",
            "GCONV_PATH",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER",
        ] {
            assert!(!project_may_set(name), "{name}");
        }
        for name in [
            "DATABASE_URL",
            "NODE_PATH",
            "npm_config_registry",
            "RUSTFLAGS",
            "CC",
            "CXX",
            "CFLAGS",
            "CARGO_TARGET_DIR",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        ] {
            assert!(project_may_set(name), "{name}");
        }
    }
}
