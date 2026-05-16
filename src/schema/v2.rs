//! JSON schema **v2** — tool-name source labels (current default).
//!
//! v2 standardized the `source` field on tool names (`"just"`, `"bacon"`,
//! `"make"`, …) instead of the filename-style strings v1 emitted. The
//! resolved values mirror [`TaskSource::label`] one-for-one — keeping
//! them aligned is a deliberate choice: v2 IS the display convention,
//! so display and JSON agree.
//!
//! If a future v3 diverges from `TaskSource::label` (e.g. a serde-friendly
//! rename), copy this file to `v3.rs`, change the strings there, and
//! freeze v2 — *never* edit these.

use crate::types::TaskSource;

/// v2 source label for a given [`TaskSource`]. Defers to
/// [`TaskSource::label`] so the display column on `runner list` and the
/// JSON `source` field stay in sync; freezing v2 if v3 diverges is a
/// matter of inlining the match here.
pub(crate) const fn source_label(source: TaskSource) -> &'static str {
    source.label()
}
