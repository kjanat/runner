//! Key paths into `runner.toml`, kept as decoded keys.

use std::fmt;

/// A `runner.toml` key path: `["tasks", "package.json:build", "pm"]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct KeyPath(pub Vec<String>);

impl KeyPath {
    /// The path to `key` under this one.
    #[must_use]
    pub(crate) fn join(&self, key: impl Into<String>) -> Self {
        let mut keys = self.0.clone();
        keys.push(key.into());
        Self(keys)
    }

    /// The decoded keys.
    pub(crate) fn keys(&self) -> &[String] {
        &self.0
    }
}

impl<const N: usize> From<[&str; N]> for KeyPath {
    fn from(keys: [&str; N]) -> Self {
        Self(keys.into_iter().map(str::to_owned).collect())
    }
}

impl fmt::Display for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, key) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            f.write_str(&toml_key(key))?;
        }
        Ok(())
    }
}

/// `name` written as a TOML key, quoted when it must be.
pub(crate) fn toml_key(name: &str) -> String {
    let mut table = toml::Table::new();
    table.insert(name.to_owned(), toml::Value::Boolean(true));
    let line = table.to_string();
    line.strip_suffix(" = true\n").unwrap_or(&line).to_owned()
}

#[cfg(test)]
mod tests {
    use super::{KeyPath, toml_key};

    #[test]
    fn keys_are_quoted_only_when_toml_requires_it() {
        assert_eq!(toml_key("build"), "build");
        assert_eq!(toml_key("build:web"), "\"build:web\"");
        assert_eq!(
            KeyPath::from(["tasks", "package.json:build", "pm"]).to_string(),
            "tasks.\"package.json:build\".pm"
        );
    }
}
