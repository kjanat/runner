//! `runner schema` — emit the `runner.toml` JSON Schema (feature `schema-gen`).

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};

/// Write the schema to `output`, or to stdout when `None`. A trailing newline
/// is appended so the committed `schemas/*.json` ends cleanly.
pub(crate) fn write_schema(output: Option<&Path>) -> Result<()> {
    let schema = crate::config_schema();
    let json = serde_json::to_string_pretty(&schema).context("failed to serialize schema")?;
    output.map_or_else(
        || writeln!(std::io::stdout(), "{json}").context("failed to write schema to stdout"),
        |path| {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("failed to write {}", path.display()))
        },
    )
}
