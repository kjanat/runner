//! `package.json` fields that name a package manager.

use runner_core::{Declared, ProviderId};
use serde_json::Value;

const NAMED: &[(&str, ProviderId)] = &[
    ("npm", ProviderId::Npm),
    ("yarn", ProviderId::Yarn),
    ("pnpm", ProviderId::Pnpm),
    ("bun", ProviderId::Bun),
    ("deno", ProviderId::Deno),
];

fn provider_named(name: &str) -> Option<ProviderId> {
    NAMED
        .iter()
        .find(|(label, _)| *label == name)
        .map(|(_, id)| *id)
}

fn declaration(name: &str, version: Option<&str>, own: ProviderId) -> Option<Declared> {
    let named = provider_named(name)?;
    if named != own {
        return Some(Declared::Alternative(named));
    }
    Some(
        version
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map_or(Declared::Named, |version| {
                Declared::Version(version.to_owned())
            }),
    )
}

/// Read the legacy `packageManager` string, `name@version`, for `own`.
#[must_use]
pub fn package_manager(value: &Value, own: ProviderId) -> Option<Declared> {
    let raw = value.as_str()?.trim();
    let (name, version) = raw.split_once('@').map_or((raw, None), |(name, version)| {
        (
            name,
            Some(version.split_once('+').map_or(version, |(v, _)| v)),
        )
    });
    declaration(name, version, own)
}

/// Read the `devEngines.packageManager` object, `{ name, version }`, for `own`.
#[must_use]
pub fn dev_engines(value: &Value, own: ProviderId) -> Option<Declared> {
    let name = value.get("name")?.as_str()?;
    let version = value.get("version").and_then(Value::as_str);
    declaration(name, version, own)
}

/// Read `engines.node`.
#[must_use]
pub fn engines_node(value: &Value) -> Option<Declared> {
    value
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| Declared::Version(v.to_owned()))
}

#[cfg(test)]
mod tests {
    use runner_core::{Declared, ProviderId};
    use serde_json::json;

    use super::{dev_engines, package_manager};

    #[test]
    fn package_manager_reports_own_version_and_alternatives() {
        assert_eq!(
            package_manager(&json!("pnpm@9.0.0+sha512.abc"), ProviderId::Pnpm),
            Some(Declared::Version("9.0.0".to_owned()))
        );
        assert_eq!(
            package_manager(&json!("pnpm@9.0.0"), ProviderId::Npm),
            Some(Declared::Alternative(ProviderId::Pnpm))
        );
        assert_eq!(
            package_manager(&json!("bun"), ProviderId::Bun),
            Some(Declared::Named)
        );
        assert_eq!(package_manager(&json!("pnpmm@9"), ProviderId::Pnpm), None);
        assert_eq!(package_manager(&json!(9), ProviderId::Pnpm), None);
    }

    #[test]
    fn dev_engines_reads_the_object_form() {
        let value = json!({ "name": "yarn", "version": "4", "onFail": "warn" });
        assert_eq!(
            dev_engines(&value, ProviderId::Yarn),
            Some(Declared::Version("4".to_owned()))
        );
        assert_eq!(
            dev_engines(&value, ProviderId::Npm),
            Some(Declared::Alternative(ProviderId::Yarn))
        );
        assert_eq!(
            dev_engines(&json!({ "version": "4" }), ProviderId::Yarn),
            None
        );
    }
}
