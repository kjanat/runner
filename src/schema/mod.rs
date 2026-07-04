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

pub(crate) mod doctor_v3;
pub(crate) mod labels;
pub(crate) mod project;
pub(crate) mod v1;
pub(crate) mod v2;
pub(crate) mod v3;

// Re-export so callers write `crate::schema::Project` rather than
// `crate::schema::project::Project`. The inner module stays public
// to crate so test files / future tooling can still reach the
// builder methods directly without going through this shim.
pub(crate) use project::Project;

/// Highest JSON schema version `list` can produce (and the version the
/// flat [`project::Project`] shape serves). Increments on any breaking
/// change to that serialized contract.
///
/// Surfaces version independently: `doctor` is at
/// [`DOCTOR_CURRENT_VERSION`] and `why` at [`WHY_CURRENT_VERSION`];
/// `list` stays here until a v3 contract for it exists.
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

/// Highest JSON schema version `doctor` can produce.
///
/// **v3** — structured diagnostic inventory ([`doctor_v3`]):
/// `invocation`/`environment`/`runner` provenance, per-ecosystem
/// decisions with confidence, first-class `sources`, `fqn`-keyed tasks,
/// PATH-probed `tools`, duplicate-name `conflicts`, flattened
/// `diagnostics`, and a self-describing `resolution` policy block.
pub(crate) const DOCTOR_CURRENT_VERSION: u32 = 3;

/// Highest JSON schema version `why` can produce.
///
/// **v3** — structured report: candidates become `{task, match}` pairs
/// carrying identity (`fqn`, `provider`, `kind`, `source`,
/// `source_pointer`), resolution data (`definition`, `resolved`, `cwd`,
/// `aliases`, `dependencies`), and the match/decision breakdown that
/// mirrors the run-time selection key. Cargo alias tasks are labeled
/// `"cargo-alias"` (see [`v3`]).
pub(crate) const WHY_CURRENT_VERSION: u32 = 3;

/// Validate that `requested` is a schema version `doctor`/`list` can
/// produce. Returns the version unchanged on success so callers can
/// chain it directly into the builder.
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

/// Validate that `requested` is a schema version `doctor` can produce.
///
/// # Errors
///
/// Returns `Err` when `requested == 0` or `requested >
/// DOCTOR_CURRENT_VERSION`, advertising the doctor-specific range.
pub(crate) fn validate_doctor_schema_version(requested: u32) -> anyhow::Result<u32> {
    if requested == 0 || requested > DOCTOR_CURRENT_VERSION {
        anyhow::bail!(
            "unsupported --schema-version {requested}; `runner doctor` speaks \
             1..={DOCTOR_CURRENT_VERSION}",
        );
    }
    Ok(requested)
}

/// Base URL committed schemas hang off, from `[package.metadata].schema-base`
/// in `Cargo.toml` (surfaced by `build.rs`). Any trailing slash is trimmed so
/// callers append `/<file>` uniformly.
fn schemas_base_url() -> &'static str {
    env!("RUNNER_SCHEMA_BASE").trim_end_matches('/')
}

/// Canonical public URL of a committed output schema.
pub(crate) fn schema_url(command: &str, version: u32) -> String {
    format!("{}/{command}.v{version}.schema.json", schemas_base_url())
}

/// Canonical URL of the `runner.toml` config schema — the committed file's
/// `$id` and the `#:schema` directive the scaffold writes.
pub(crate) fn config_schema_url() -> String {
    format!("{}/runner.toml.schema.json", schemas_base_url())
}

/// Validate that `requested` is a schema version `why` can produce.
///
/// # Errors
///
/// Returns `Err` when `requested == 0` or `requested >
/// WHY_CURRENT_VERSION`, advertising the why-specific supported range.
pub(crate) fn validate_why_schema_version(requested: u32) -> anyhow::Result<u32> {
    if requested == 0 || requested > WHY_CURRENT_VERSION {
        anyhow::bail!(
            "unsupported --schema-version {requested}; `runner why` speaks \
             1..={WHY_CURRENT_VERSION}",
        );
    }
    Ok(requested)
}

#[cfg(test)]
mod tests {
    use super::{
        CURRENT_VERSION, DOCTOR_CURRENT_VERSION, WHY_CURRENT_VERSION, labels::source_label_for,
        validate_doctor_schema_version, validate_schema_version, validate_why_schema_version,
    };
    use crate::types::TaskSource;

    #[test]
    fn every_schema_label_round_trips_through_from_label() {
        // `doctor --json` / `why --json` print FQNs built from these
        // labels, and `run` parses FQN input back through
        // `TaskSource::from_label`. Every label of every schema version
        // must round-trip, or the printed identity is unrunnable and the
        // token leaks to the PM-exec fallback (bunx resolving it off the
        // network) — the v3 `cargo-alias` label shipped exactly that bug.
        for version in 1..=CURRENT_VERSION {
            for &source in TaskSource::all() {
                let label = source_label_for(source, version);
                assert_eq!(
                    TaskSource::from_label(label),
                    Some(source),
                    "schema v{version} label {label:?} must parse back to {source:?}",
                );
            }
        }
    }

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
        // Regression guard: `CURRENT_VERSION` (doctor/list) and the v2
        // module must stay in lock-step until their v3 contracts are
        // reviewed and implemented; `why` versions independently via
        // `WHY_CURRENT_VERSION`.
        assert_eq!(CURRENT_VERSION, 2);
        assert_eq!(
            source_label_for(TaskSource::Justfile, CURRENT_VERSION),
            "just"
        );
    }

    #[test]
    fn why_version_matches_v3_labels() {
        // v3's single label divergence: cargo aliases name the
        // mechanism, freeing `provider` to carry `"cargo"`.
        assert_eq!(WHY_CURRENT_VERSION, 3);
        assert_eq!(
            source_label_for(TaskSource::CargoAliases, WHY_CURRENT_VERSION),
            "cargo-alias"
        );
        // Everything else inherits v2 unchanged.
        assert_eq!(
            source_label_for(TaskSource::Justfile, WHY_CURRENT_VERSION),
            "just"
        );
        assert_eq!(
            source_label_for(TaskSource::PackageJson, WHY_CURRENT_VERSION),
            "package.json"
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

        // doctor/list do not speak v3 yet — only `why` does.
        let err = validate_schema_version(3).expect_err("doctor/list must reject v3");
        assert!(format!("{err}").contains("1..=2"));
    }

    #[test]
    fn validate_doctor_schema_version_spans_one_through_three() {
        assert_eq!(DOCTOR_CURRENT_VERSION, 3);
        assert_eq!(validate_doctor_schema_version(1).unwrap(), 1);
        assert_eq!(validate_doctor_schema_version(2).unwrap(), 2);
        assert_eq!(validate_doctor_schema_version(3).unwrap(), 3);

        let err = validate_doctor_schema_version(0).expect_err("v0 must error");
        assert!(format!("{err}").contains("unsupported"));
        let err = validate_doctor_schema_version(4).expect_err("future versions must error");
        assert!(
            format!("{err}").contains("1..=3"),
            "error should advertise the doctor range",
        );
    }

    #[test]
    fn validate_why_schema_version_spans_one_through_three() {
        assert_eq!(validate_why_schema_version(1).unwrap(), 1);
        assert_eq!(validate_why_schema_version(2).unwrap(), 2);
        assert_eq!(validate_why_schema_version(3).unwrap(), 3);

        let err = validate_why_schema_version(0).expect_err("v0 must error");
        assert!(format!("{err}").contains("unsupported"));
        let err = validate_why_schema_version(4).expect_err("future versions must error");
        assert!(
            format!("{err}").contains("1..=3"),
            "error should advertise the why range",
        );
    }
}
