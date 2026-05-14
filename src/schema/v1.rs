//! JSON schema **v1** — legacy filename-style source labels.
//!
//! Frozen contract: never edit these strings. Anything reading the JSON
//! with `--schema-version=1` was written against these exact values and
//! will break if they drift. New label conventions go in a fresh `vN.rs`
//! plus a new arm in [`super::labels::source_label_for`].
//!
//! v1 was the original schema (issued at runner 0.10.x) and remains
//! supported indefinitely as long as the `TaskSource` enum can still
//! be mapped to these strings. When a `TaskSource` variant is *removed*,
//! the v1 mapping for it can collapse to `"<removed>"` or similar — but
//! that's a v3+ design question; today every variant has a stable v1
//! string.

use crate::types::TaskSource;

/// v1 source label for a given [`TaskSource`]. Mirrors the strings the
/// original `runner doctor --json` output emitted.
pub(crate) const fn source_label(source: TaskSource) -> &'static str {
    match source {
        TaskSource::PackageJson => "package.json",
        TaskSource::Makefile => "Makefile",
        TaskSource::Justfile => "justfile",
        TaskSource::Taskfile => "Taskfile",
        TaskSource::TurboJson => "turbo.json",
        TaskSource::DenoJson => "deno.json",
        TaskSource::CargoAliases => "cargo",
        TaskSource::BaconToml => "bacon.toml",
        TaskSource::MiseToml => "mise.toml",
    }
}
