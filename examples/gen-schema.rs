//! Emit the JSON Schema for `runner.toml` to disk.
//!
//! Run via:
//!
//! ```bash
//! cargo run --example gen-schema --features schema-gen
//! ```
//!
//! Writes to `schemas/runner.toml.schema.json` (relative to the
//! workspace root) and prints a one-line summary on stdout. CI can
//! verify the committed schema is in sync by re-running the example
//! and asserting `git diff --exit-code schemas/`.
//!
//! Schema is committed to the repo so editors with a TOML language
//! server (Taplo, Even Better TOML, etc.) can validate `runner.toml`
//! without any external dependency on this binary or schemastore.

use std::fs;
use std::path::Path;

fn main() {
    let schema = runner::config_schema();
    let json = serde_json::to_string_pretty(&schema).expect("schema should serialize to JSON");

    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/runner.toml.schema.json");
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("schemas dir should be creatable");
    }
    // Keep a trailing newline so the committed file plays nice with
    // editor "ensure newline at EOF" rules and `git diff` doesn't
    // show "\ No newline at end of file".
    fs::write(&out, format!("{json}\n")).expect("schema file should be writable");

    println!("wrote {} ({} bytes)", out.display(), json.len() + 1);
}
