//! Pretty JSON on stdout.

use anyhow::Result;
use serde::Serialize;

/// Print `value` as indented JSON followed by a newline.
///
/// # Errors
///
/// When `value` fails to serialize.
pub(crate) fn print<T: Serialize>(value: &T) -> Result<()> {
    write(&mut std::io::stdout(), value)
}

/// Write `value` as indented JSON followed by a newline.
///
/// # Errors
///
/// When `value` fails to serialize or `out` fails to take it.
pub(crate) fn write<T: Serialize>(out: &mut dyn std::io::Write, value: &T) -> Result<()> {
    writeln!(out, "{}", serde_json::to_string_pretty(value)?)?;
    Ok(())
}
