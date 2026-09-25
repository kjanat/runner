//! The declaration table: every setting, declared once.

/// What a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// A provider label.
    Provider,
    /// A provider label per ecosystem.
    ProviderPerEcosystem,
    /// `true` or `false`.
    Bool,
    /// One of a closed set of labels.
    Choice(&'static [&'static str]),
    /// A `KEY = "value"` table.
    EnvTable,
    /// A list of operation names, per tool manager.
    Operations,
}

/// One row: a config key, its env var, its CLI flag, its doc line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    /// The `runner.toml` key.
    pub key: &'static str,
    /// The `RUNNER_*` variable, when the setting has one.
    pub env: Option<&'static str>,
    /// The long flag, when the setting has one.
    pub flag: Option<&'static str>,
    /// What the setting holds.
    pub kind: SettingKind,
    /// Whether `runner.toml` accepts the key.
    pub config: bool,
    /// The one line schema, completion and docs print.
    pub doc: &'static str,
}

/// The policy fields of section 4.5, one row each.
pub static SETTINGS: &[Setting] = &[
    Setting {
        key: "pm",
        env: Some("RUNNER_PM"),
        flag: Some("pm"),
        kind: SettingKind::ProviderPerEcosystem,
        config: true,
        doc: "The package manager that installs and runs scripts, per ecosystem.",
    },
    Setting {
        key: "tasks.prefer",
        env: Some("RUNNER_RUNNER"),
        flag: Some("runner"),
        kind: SettingKind::Provider,
        config: true,
        doc: "The task source that wins a same-named task.",
    },
    Setting {
        key: "runtime.js",
        env: Some("RUNNER_RUNTIME"),
        flag: Some("runtime"),
        kind: SettingKind::Provider,
        config: true,
        doc: "The JavaScript runtime scripts and files run on.",
    },
    Setting {
        key: "defaults.frozen",
        env: None,
        flag: Some("frozen"),
        kind: SettingKind::Bool,
        config: true,
        doc: "Install without touching the lockfile.",
    },
    Setting {
        key: "install.scripts",
        env: Some("RUNNER_INSTALL_SCRIPTS"),
        flag: Some("scripts"),
        kind: SettingKind::Choice(&["deny", "allow"]),
        config: true,
        doc: "What an install does with lifecycle scripts.",
    },
    Setting {
        key: "defaults.fetch",
        env: Some("RUNNER_REACH"),
        flag: Some("fetch"),
        kind: SettingKind::Choice(&["ask", "allow", "local"]),
        config: true,
        doc: "Whether a command that can download may run.",
    },
    Setting {
        key: "defaults.verbosity",
        env: Some("RUNNER_QUIET"),
        flag: Some("quiet"),
        kind: SettingKind::Choice(&["normal", "quiet", "very-quiet", "silent"]),
        config: false,
        doc: "How much of a host's own output to suppress.",
    },
    Setting {
        key: "env",
        env: None,
        flag: None,
        kind: SettingKind::EnvTable,
        config: true,
        doc: "Variables every command gets.",
    },
    Setting {
        key: "tools.<name>.env",
        env: None,
        flag: None,
        kind: SettingKind::EnvTable,
        config: true,
        doc: "Variables one tool's commands get.",
    },
    Setting {
        key: "tasks.<name>.env",
        env: None,
        flag: None,
        kind: SettingKind::EnvTable,
        config: true,
        doc: "Variables one task gets.",
    },
    Setting {
        key: "tools.<name>.install",
        env: None,
        flag: None,
        kind: SettingKind::Operations,
        config: true,
        doc: "The operations a tool manager runs on install.",
    },
];

impl Setting {
    /// The row for `key`.
    #[must_use]
    pub fn by_key(key: &str) -> Option<&'static Self> {
        SETTINGS.iter().find(|setting| setting.key == key)
    }

    /// The CLI flag declared for a policy key.
    ///
    /// # Panics
    /// Panics for keys without a declared flag.
    #[must_use]
    pub fn flag_for(key: &str) -> &'static str {
        Self::by_key(key)
            .and_then(|setting| setting.flag)
            .expect("declared CLI policy flag")
    }

    /// The environment variable declared for a policy key.
    ///
    /// # Panics
    /// Panics for keys without a declared environment variable.
    #[must_use]
    pub fn env_for(key: &str) -> &'static str {
        Self::by_key(key)
            .and_then(|setting| setting.env)
            .expect("declared policy environment variable")
    }

    /// The doc line declared for a policy key.
    ///
    /// # Panics
    /// Panics for undeclared keys.
    #[must_use]
    pub fn doc_for(key: &str) -> &'static str {
        Self::by_key(key).expect("declared policy key").doc
    }

    /// The labels a [`SettingKind::Choice`] key accepts.
    ///
    /// # Panics
    /// Panics for keys that are not a closed choice.
    #[must_use]
    pub fn choices_for(key: &str) -> &'static [&'static str] {
        match Self::by_key(key).map(|setting| setting.kind) {
            Some(SettingKind::Choice(choices)) => choices,
            _ => panic!("{key} is not a declared choice"),
        }
    }

    /// The row for the `RUNNER_*` variable `env`.
    #[must_use]
    pub fn by_env(env: &str) -> Option<&'static Self> {
        SETTINGS.iter().find(|setting| setting.env == Some(env))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{SETTINGS, Setting};

    #[test]
    fn keys_env_vars_and_flags_are_each_declared_once() {
        let mut keys = HashSet::new();
        let mut envs = HashSet::new();
        let mut flags = HashSet::new();
        for setting in SETTINGS {
            assert!(keys.insert(setting.key), "{} declared twice", setting.key);
            if let Some(env) = setting.env {
                assert!(
                    env.starts_with("RUNNER_"),
                    "{env} is not a RUNNER_ variable"
                );
                assert!(envs.insert(env), "{env} declared twice");
            }
            if let Some(flag) = setting.flag {
                assert!(flags.insert(flag), "{flag} declared twice");
            }
            assert!(
                setting.doc.ends_with('.'),
                "{}: doc is not a sentence",
                setting.key
            );
        }
        assert_eq!(Setting::by_env("RUNNER_PM").map(|s| s.key), Some("pm"));
        assert!(Setting::by_key("nothing").is_none());
    }
}
