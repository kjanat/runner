//! Pretty JSON on stdout.

use anyhow::Result;
use serde::Serialize;

/// Print `value` as indented JSON followed by a newline.
///
/// # Errors
///
/// When `value` fails to serialize.
pub(crate) fn print<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
