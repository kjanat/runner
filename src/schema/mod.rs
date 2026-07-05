//! JSON schema for `--json` output.
//!
//! # Layout
//!
//! - [`SCHEMA_VERSION`] — the schema contract version every `--json` surface stamps into its `schema_version` field.
//! - [`validate_schema_version`] — gatekeeper for `--schema-version=N`; `clap` already bounds the flag to `1..=1`,
//!   so this is a defensive second check for callers that build a version outside CLI parsing.
//! - [`project`] — the flat JSON shape ([`Project`], [`TaskListView`]) served by `list`/`info`
//!   (and read internally by `doctor`'s human renderer).
//! - [`doctor`] — the structured `doctor --json` report.
//! - [`labels::flat_source_label`] / [`labels::structured_source_label`] — the two source-label conventions the shapes above use.
//!
//! There used to be three independently-versioned schemas (`list` at v2, `doctor`/`why` at v3, with v1 the original filename-style labels).
//! Adoption never grew past internal use, so the versions were collapsed: today's shapes are the only ones, retroactively called v1.

pub(crate) mod doctor;
pub(crate) mod labels;
pub(crate) mod project;

// Re-export so callers write `crate::schema::Project` rather than `crate::schema::project::Project`.
// The inner module stays public to crate so test files / future tooling can still reach the builder methods directly without going through this shim.
pub(crate) use project::Project;

/// Schema contract version every `--json` surface (`doctor`, `list`, `why`) stamps into its `schema_version` field.
/// Bump whenever any field's serialized representation changes in a way clients can observe (rename, type change, removed field, etc.).
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Validate that `requested` is a schema version this binary can produce.
/// Returns the version unchanged on success so callers can chain it directly into a builder.
///
/// # Errors
///
/// Returns `Err` when `requested != SCHEMA_VERSION`. `clap` already bounds `--schema-version` to `1..=1`,
/// so this only fires for callers that construct a version outside CLI parsing.
pub(crate) fn validate_schema_version(requested: u32) -> anyhow::Result<u32> {
    if requested != SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported --schema-version {requested}; this binary speaks {SCHEMA_VERSION}",
        );
    }
    Ok(requested)
}

/// Base URL committed schemas hang off, from `[package.metadata].schema-base` in `Cargo.toml` (surfaced by `build.rs`).
/// Any trailing slash is trimmed so callers append `/<file>` uniformly.
fn schemas_base_url() -> &'static str {
    env!("RUNNER_SCHEMA_BASE").trim_end_matches('/')
}

/// Canonical public URL of a committed output schema.
pub(crate) fn schema_url(command: &str) -> String {
    format!("{}/{command}.schema.json", schemas_base_url())
}

/// Canonical URL of the `runner.toml` config schema — the committed file's `$id` and the `#:schema` directive the scaffold writes.
pub(crate) fn config_schema_url() -> String {
    format!("{}/runner.toml.schema.json", schemas_base_url())
}

#[cfg(test)]
mod tests {
    use super::{SCHEMA_VERSION, validate_schema_version};

    #[test]
    fn validate_schema_version_accepts_only_the_current_version() {
        assert_eq!(validate_schema_version(1).unwrap(), 1);
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn validate_schema_version_rejects_anything_else() {
        let err = validate_schema_version(0).expect_err("v0 must error");
        assert!(format!("{err}").contains("unsupported"));

        let err = validate_schema_version(2).expect_err("v2 no longer exists");
        let msg = format!("{err}");
        assert!(msg.contains("unsupported"));
        assert!(
            msg.contains("speaks 1"),
            "error should advertise the supported version: {msg}",
        );
    }
}
