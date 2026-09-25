//! `package.json` fields that name a package manager, and the check they ask for.

use runner_core::{
    Check, Declared, Field, Layer, OnFail, Op, Policy, Present, ProviderId, Refusal, Tree, Warning,
    check, decided_by, scope_dir,
};
use runner_schemes::NodeSemver;
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
    Some(trimmed(version).map_or(Declared::Named, |version| {
        Declared::Version(version.to_owned())
    }))
}

fn trimmed(version: Option<&str>) -> Option<&str> {
    version.map(str::trim).filter(|v| !v.is_empty())
}

/// Read the legacy `packageManager` string, `name@version`, for `own`.
#[must_use]
pub fn package_manager(field: &Field<'_>, own: ProviderId) -> Option<Declared> {
    let raw = field.value.as_str()?.trim();
    let (name, version) = raw.split_once('@').map_or((raw, None), |(name, version)| {
        (
            name,
            Some(version.split_once('+').map_or(version, |(v, _)| v)),
        )
    });
    declaration(name, version, own)
}

/// Read `devEngines.packageManager`, one object or a list of them, for `own`.
///
/// The last entry naming a known package manager wins. Its `onFail` defaults
/// to `error` when it is the last such entry and `ignore` otherwise, as the
/// `OpenJS` proposal specifies; `download` reads as `warn`.
#[must_use]
pub fn dev_engines(field: &Field<'_>, own: ProviderId) -> Option<Declared> {
    if legacy_field_is_unreadable(field.manifest) {
        return None;
    }
    let entries: Vec<&Value> = match field.value {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(_) => vec![field.value],
        _ => return None,
    };
    let known: Vec<(&Value, ProviderId)> = entries
        .into_iter()
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .and_then(provider_named)
                .map(|id| (entry, id))
        })
        .collect();
    let (entry, named) = *known.last()?;
    if named != own {
        return Some(Declared::Alternative(named));
    }
    let on_fail = match entry.get("onFail").and_then(Value::as_str) {
        Some("ignore") => OnFail::Ignore,
        Some("warn" | "download") => OnFail::Warn,
        _ => OnFail::Error,
    };
    Some(Declared::Constraint {
        version: trimmed(entry.get("version").and_then(Value::as_str)).map(str::to_owned),
        on_fail,
    })
}

/// Read `engines.node`.
#[must_use]
pub fn engines_node(field: &Field<'_>) -> Option<Declared> {
    trimmed(field.value.as_str()).map(|v| Declared::Version(v.to_owned()))
}

/// Whether `packageManager` holds a nonempty value that names no known
/// manager, which voids `devEngines.packageManager` as well.
fn legacy_field_is_unreadable(manifest: &Value) -> bool {
    manifest
        .get("packageManager")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|raw| {
            !raw.is_empty()
                && provider_named(raw.split_once('@').map_or(raw, |(name, _)| name)).is_none()
        })
}

/// Check the requirement `devEngines.packageManager` declares for this provider.
///
/// The check runs only when that field is what selected the provider; a
/// `packageManager` field or a policy layer above the manifest settles the
/// choice by itself.
///
/// # Errors
/// Refuses when the manifest asks for `error` and the executable is absent or
/// its version violates the constraint.
pub fn before_plan(
    tree: &Tree,
    present: &Present,
    _: &Op<'_>,
    policy: &Policy,
    warnings: &mut Vec<Warning>,
) -> Result<(), Refusal> {
    if decided_by(policy, present)
        .first()
        .is_some_and(|layer| matches!(layer, Layer::Cli | Layer::Env | Layer::ConfigFile(_)))
    {
        return Ok(());
    }
    let Some((version, on_fail)) = present.because.first().and_then(|e| match &e.declared {
        Some(Declared::Constraint { version, on_fail }) => Some((version.as_deref(), *on_fail)),
        _ => None,
    }) else {
        return Ok(());
    };
    if on_fail == OnFail::Ignore {
        return Ok(());
    }
    let provider = crate::REGISTRY.by_id(present.provider);
    let program = provider.program.unwrap_or(provider.label);
    if runner_core::probe_with(program, &present.bin_dirs).is_none() {
        return outcome(
            on_fail,
            provider.id,
            format!(
                "devEngines.packageManager declares {} but it was not found on PATH",
                provider.label
            ),
            warnings,
        );
    }
    let Some(declared) = version else {
        return Ok(());
    };
    let found = present.version.clone().map_or_else(
        || {
            crate::version::read(&scope_dir(tree, &present.scope), present)
                .map_err(|warning| warning.message)
        },
        Ok,
    );
    let checked = found.map_or_else(
        |reason| Check::Unknown { reason },
        |found| check::<NodeSemver>(declared, &found),
    );
    match checked {
        Check::Satisfied => Ok(()),
        Check::Violated { declared, found } => outcome(
            on_fail,
            provider.id,
            format!(
                "devEngines.packageManager requires {} {declared} but the installed version is \
                 {found}",
                provider.label
            ),
            warnings,
        ),
        Check::Unknown { reason } => {
            warnings.push(Warning::about(
                provider.id,
                format!(
                    "cannot evaluate {} version constraint {declared}: {reason}",
                    provider.label
                ),
            ));
            Ok(())
        }
    }
}

fn outcome(
    on_fail: OnFail,
    provider: ProviderId,
    message: String,
    warnings: &mut Vec<Warning>,
) -> Result<(), Refusal> {
    match on_fail {
        OnFail::Error => Err(Refusal::Invalid(format!("{message} (onFail=error)"))),
        OnFail::Warn | OnFail::Ignore => {
            warnings.push(Warning::about(provider, message));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use runner_core::{Declared, Field, OnFail, ProviderId};
    use serde_json::{Value, json};

    static LONE: Value = Value::Null;

    fn package_manager(value: &Value, own: ProviderId) -> Option<Declared> {
        super::package_manager(
            &Field {
                value,
                manifest: &LONE,
            },
            own,
        )
    }

    fn dev_engines(value: &Value, own: ProviderId) -> Option<Declared> {
        super::dev_engines(
            &Field {
                value,
                manifest: &LONE,
            },
            own,
        )
    }

    #[test]
    fn an_unreadable_legacy_field_voids_dev_engines() {
        let manifest = json!({
            "packageManager": "pnpmm@9",
            "devEngines": { "packageManager": { "name": "yarn", "onFail": "ignore" } }
        });
        let field = Field {
            value: &manifest["devEngines"]["packageManager"],
            manifest: &manifest,
        };
        assert_eq!(super::dev_engines(&field, ProviderId::Yarn), None);
        let empty = json!({
            "packageManager": "  ",
            "devEngines": { "packageManager": { "name": "yarn", "onFail": "ignore" } }
        });
        let field = Field {
            value: &empty["devEngines"]["packageManager"],
            manifest: &empty,
        };
        assert_eq!(
            super::dev_engines(&field, ProviderId::Yarn),
            Some(Declared::Constraint {
                version: None,
                on_fail: OnFail::Ignore,
            })
        );
    }

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
            Some(Declared::Constraint {
                version: Some("4".to_owned()),
                on_fail: OnFail::Warn,
            })
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

    #[test]
    fn a_single_entry_defaults_to_error_and_download_reads_as_warn() {
        assert_eq!(
            dev_engines(&json!({ "name": "pnpm" }), ProviderId::Pnpm),
            Some(Declared::Constraint {
                version: None,
                on_fail: OnFail::Error,
            })
        );
        assert_eq!(
            dev_engines(
                &json!({ "name": "pnpm", "onFail": "download" }),
                ProviderId::Pnpm
            ),
            Some(Declared::Constraint {
                version: None,
                on_fail: OnFail::Warn,
            })
        );
    }

    #[test]
    fn the_last_known_entry_of_a_list_wins_with_its_own_on_fail() {
        let value = json!([
            { "name": "pnpm", "version": ">=9" },
            { "name": "yarn", "onFail": "warn" },
            { "name": "cargo" }
        ]);
        assert_eq!(
            dev_engines(&value, ProviderId::Yarn),
            Some(Declared::Constraint {
                version: None,
                on_fail: OnFail::Warn,
            })
        );
        assert_eq!(
            dev_engines(&value, ProviderId::Pnpm),
            Some(Declared::Alternative(ProviderId::Yarn))
        );
        let trailing_unknown = json!([{ "name": "pnpm" }, { "name": "cargo" }]);
        assert_eq!(
            dev_engines(&trailing_unknown, ProviderId::Pnpm),
            Some(Declared::Constraint {
                version: None,
                on_fail: OnFail::Error,
            })
        );
        assert_eq!(
            dev_engines(&json!([{ "name": "cargo" }]), ProviderId::Npm),
            None
        );
        assert_eq!(dev_engines(&json!("pnpm@9"), ProviderId::Pnpm), None);
    }
}
