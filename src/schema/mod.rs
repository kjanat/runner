//! Versioned JSON schema for `--json` output.
//!
//! # Layout
//!
//! - [`CURRENT_VERSION`] — the latest schema this binary can produce.
//!   Bump whenever any field's serialized representation changes in a
//!   way clients can observe (rename, type change, removed field, etc.).
//! - [`validate_schema_version`] — gatekeeper for `--schema-version=N`;
//!   rejects values outside `1..=CURRENT_VERSION` with a clean error.
//! - [`project`] — the actual JSON-shape types ([`Project`],
//!   [`TaskListView`], …) and the builder switch
//!   [`Project::build_with_schema`].
//! - [`labels::source_label_for`] — the version → label-string dispatcher
//!   that the project builder routes through for every `source` field.
//! - [`v1`] / [`v2`] / future [`vN`] — frozen label tables, one file per
//!   schema version. Adding a new version is mechanical: copy the most
//!   recent `vN.rs`, edit the strings, add one arm to
//!   [`labels::source_label_for`], bump [`CURRENT_VERSION`].
//!
//! # When to bump the version
//!
//! Adding a field is *not* a breaking change — clients can ignore
//! unknown fields. Renaming or removing one is. The current label
//! convention (filename-style → tool names) was a rename, so it moved
//! from v1 to v2.

pub(crate) mod labels;
pub(crate) mod project;
pub(crate) mod v1;
pub(crate) mod v2;

// Re-export so callers write `crate::schema::Project` rather than
// `crate::schema::project::Project`. The inner module stays public
// to crate so test files / future tooling can still reach the
// builder methods directly without going through this shim.
pub(crate) use project::Project;

/// Highest JSON schema version this binary can produce. Increments on
/// any breaking change to the serialized contract.
///
/// **v2** — source labels standardized to tool names (`"just"`,
/// `"bacon"`, `"make"`, `"turbo"`, `"deno"`, `"task"`, `"mise"`).
/// `"package.json"` and `"cargo"` unchanged. Consumers reading
/// `decisions.*.source` or `tasks[].source` from a v2 payload need to
/// recognize the tool-name strings.
///
/// **v1** — original schema, filename-style source labels
/// (`"justfile"`, `"bacon.toml"`, …). Still produced when callers pass
/// `--schema-version=1`.
pub(crate) const CURRENT_VERSION: u32 = 2;

/// Validate that `requested` is a schema version this binary can produce.
/// Returns the version unchanged on success so callers can chain it
/// directly into the builder.
///
/// # Errors
///
/// Returns `Err` when `requested == 0` or `requested > CURRENT_VERSION`.
/// The error message advertises the supported range so client scripts
/// can adapt.
pub(crate) fn validate_schema_version(requested: u32) -> anyhow::Result<u32> {
    if requested == 0 || requested > CURRENT_VERSION {
        anyhow::bail!(
            "unsupported --schema-version {requested}; this binary speaks 1..={CURRENT_VERSION}",
        );
    }
    Ok(requested)
}

#[cfg(test)]
mod tests {
    use super::{CURRENT_VERSION, labels::source_label_for, validate_schema_version};
    use crate::types::TaskSource;

    #[test]
    fn source_label_for_returns_legacy_strings_under_v1() {
        // v1 contract: filename-style labels. Frozen.
        assert_eq!(source_label_for(TaskSource::Justfile, 1), "justfile");
        assert_eq!(source_label_for(TaskSource::BaconToml, 1), "bacon.toml");
        assert_eq!(source_label_for(TaskSource::MiseToml, 1), "mise.toml");
        assert_eq!(source_label_for(TaskSource::Makefile, 1), "Makefile");
        assert_eq!(source_label_for(TaskSource::TurboJson, 1), "turbo.json");
        assert_eq!(source_label_for(TaskSource::DenoJson, 1), "deno.json");
        assert_eq!(source_label_for(TaskSource::Taskfile, 1), "Taskfile");
        // Unchanged across versions:
        assert_eq!(source_label_for(TaskSource::CargoAliases, 1), "cargo");
        assert_eq!(source_label_for(TaskSource::GoPackage, 1), "go");
        assert_eq!(source_label_for(TaskSource::PackageJson, 1), "package.json");
    }

    #[test]
    fn source_label_for_returns_tool_names_under_v2() {
        assert_eq!(source_label_for(TaskSource::Justfile, 2), "just");
        assert_eq!(source_label_for(TaskSource::BaconToml, 2), "bacon");
        assert_eq!(source_label_for(TaskSource::MiseToml, 2), "mise");
        assert_eq!(source_label_for(TaskSource::Makefile, 2), "make");
        assert_eq!(source_label_for(TaskSource::TurboJson, 2), "turbo");
        assert_eq!(source_label_for(TaskSource::DenoJson, 2), "deno");
        assert_eq!(source_label_for(TaskSource::Taskfile, 2), "task");
        assert_eq!(source_label_for(TaskSource::CargoAliases, 2), "cargo");
        assert_eq!(source_label_for(TaskSource::GoPackage, 2), "go");
        assert_eq!(source_label_for(TaskSource::PackageJson, 2), "package.json");
    }

    #[test]
    fn current_version_matches_v2_labels() {
        // Regression guard: `CURRENT_VERSION` and the v2 module must
        // stay in lock-step. If a future v3 lands, this test moves to
        // assert against `v3::source_label` and `CURRENT_VERSION = 3`.
        assert_eq!(CURRENT_VERSION, 2);
        assert_eq!(
            source_label_for(TaskSource::Justfile, CURRENT_VERSION),
            "just"
        );
    }

    #[test]
    fn validate_schema_version_accepts_supported_range() {
        assert_eq!(validate_schema_version(1).unwrap(), 1);
        assert_eq!(validate_schema_version(2).unwrap(), 2);
    }

    #[test]
    fn validate_schema_version_rejects_zero_and_future_versions() {
        let err = validate_schema_version(0).expect_err("v0 must error");
        assert!(format!("{err}").contains("unsupported"));

        let err = validate_schema_version(99).expect_err("future versions must error");
        let msg = format!("{err}");
        assert!(msg.contains("unsupported"));
        assert!(
            msg.contains("1..=2"),
            "error should advertise the supported range: {msg}",
        );
    }
}
