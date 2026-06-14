//! JSON schema **v3** — source labels for the structured reports.
//!
//! v3 applies to `runner why --json` ([`super::WHY_CURRENT_VERSION`])
//! and `runner doctor --json` ([`super::DOCTOR_CURRENT_VERSION`]);
//! `list` remains capped at [`super::CURRENT_VERSION`] until a v3
//! contract for it exists. The one label change: cargo alias tasks report
//! `"cargo-alias"` instead of `"cargo"`, so the `kind` field names the
//! *mechanism* (an `[alias]` table entry) rather than colliding with the
//! `provider` field, which already carries `"cargo"`. Every other label
//! defers to the frozen v2 table.

use crate::types::TaskSource;

/// v3 source label for a given [`TaskSource`]. Only
/// [`TaskSource::CargoAliases`] diverges from v2; see module docs.
pub(crate) const fn source_label(source: TaskSource) -> &'static str {
    match source {
        TaskSource::CargoAliases => "cargo-alias",
        _ => super::v2::source_label(source),
    }
}
