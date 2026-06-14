//! Per-version label dispatcher.
//!
//! `source_label_for(source, version)` is the only public seam between
//! [`crate::schema::project`] (which serializes the `source` field on
//! tasks and decisions) and the per-version label tables in [`super::v1`]
//! / [`super::v2`]. Adding a new schema version means: add a `vN.rs` file
//! with a `source_label` fn, add one match arm here, bump
//! [`super::CURRENT_VERSION`].
//!
//! Versions newer than the highest-known one fall through to the latest
//! match arm — never silently misrepresent: callers must validate via
//! [`super::validate_schema_version`] *before* serializing.

use crate::types::TaskSource;

/// Resolve the JSON `source` string for a given source + schema version.
///
/// Validation lives in [`super::validate_schema_version`] — by the time
/// this is called the version is already proven to be in the supported
/// range. The wildcard arm picks the newest version's labels so newly
/// added versions don't need a default branch.
pub(crate) const fn source_label_for(source: TaskSource, schema_version: u32) -> &'static str {
    match schema_version {
        1 => super::v1::source_label(source),
        2 => super::v2::source_label(source),
        _ => super::v3::source_label(source),
    }
}

/// Build a task's fully-qualified name: `<scope>:<kind>#<name>`.
///
/// The `#` boundary separates the colon-joined structured prefix
/// (`scope:kind`, both colon-free) from the verbatim task name, which may
/// itself contain `:` (e.g. an npm script `fmt:update`). Consumers split
/// once on `#`: everything after is the name, unescaped. Centralised here
/// so `why` and `doctor` can't drift apart on the format.
pub(crate) fn fqn(source: TaskSource, name: &str, schema_version: u32) -> String {
    format!(
        "root:{kind}#{name}",
        kind = source_label_for(source, schema_version)
    )
}
