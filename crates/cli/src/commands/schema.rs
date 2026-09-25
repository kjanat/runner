//! `runner schema`, emit committed JSON Schemas (feature `schema`).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use schemars::JsonSchema;
use serde_json::Value;

use crate::schema::doctor::DoctorReport;
use crate::schema::project::TaskListView;

const SCHEMA_DIR: &str = "schemas";

struct Document {
    filename: &'static str,
    json: String,
}

/// Write the config schema to stdout/a file, or every committed schema to a directory.
/// A trailing newline is appended so committed `schemas/*.json` ends cleanly.
pub(crate) fn write_schema(all: bool, output: Option<&Path>) -> Result<()> {
    if all {
        let dir = output.unwrap_or_else(|| Path::new(SCHEMA_DIR));
        write_all_schemas(dir)
    } else {
        write_json(output, &schema_json(config_schema())?)
    }
}

/// The `runner.toml` config schema.
pub(crate) fn config_schema() -> Value {
    crate::config::schema().clone()
}

fn write_all_schemas(dir: &Path) -> Result<()> {
    if dir.exists() && !dir.is_dir() {
        bail!("--all output must be a directory: {}", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    for document in documents()? {
        write_json(Some(&dir.join(document.filename)), &document.json)?;
    }

    Ok(())
}

/// The `runner.toml` that `runner config init` writes: the schema pragma,
/// then every section whose fields declare a default, with those defaults
/// live. Fields without a default and open-map sections are left out.
pub(crate) fn render_init() -> String {
    let schema = crate::config::schema();
    let mut out = format!("#:schema {}\n", crate::schema::config_schema_url());
    let Some(sections) = schema["properties"].as_object() else {
        return out;
    };
    for section in sections.keys() {
        let Some(def) = crate::config::section_def(section) else {
            continue;
        };
        let Some(fields) = schema["$defs"][def]["properties"].as_object() else {
            continue;
        };
        let lines: Vec<String> = fields
            .iter()
            .filter_map(|(field, field_schema)| {
                let default = field_schema.get("default")?;
                let value = toml::Value::try_from(default).ok()?;
                Some(format!("{field} = {value}"))
            })
            .collect();
        if lines.is_empty() {
            continue;
        }
        let _ = write!(out, "\n[{section}]\n{}\n", lines.join("\n"));
    }
    out
}

fn documents() -> Result<Vec<Document>> {
    Ok(vec![
        Document {
            filename: "runner.toml.schema.json",
            json: schema_json(config_schema())?,
        },
        Document {
            filename: "doctor.schema.json",
            json: schema_json(output_schema::<DoctorReport<'static>>()?)?,
        },
        Document {
            filename: "doctor.example.json",
            json: serde_json::to_string_pretty(&DoctorReport::example())
                .context("failed to serialize doctor example")?,
        },
        Document {
            filename: "list.schema.json",
            json: schema_json(output_schema::<TaskListView<'static>>()?)?,
        },
        Document {
            filename: "why.schema.json",
            json: schema_json(output_schema::<super::why::WhyReport<'static>>()?)?,
        },
    ])
}

fn output_schema<T: JsonSchema>() -> Result<Value> {
    let generator = schemars::generate::SchemaSettings::default()
        .for_serialize()
        .into_generator();
    serde_json::to_value(generator.into_root_schema_for::<T>())
        .context("failed to serialize schema")
}

fn schema_json(mut schema: Value) -> Result<String> {
    json_schema_sort::sort_schema(&mut schema);
    serde_json::to_string_pretty(&schema).context("failed to serialize schema")
}

fn write_json(output: Option<&Path>, json: &str) -> Result<()> {
    output.map_or_else(
        || writeln!(std::io::stdout(), "{json}").context("failed to write schema to stdout"),
        |path| {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("failed to write {}", path.display()))
        },
    )
}

#[cfg(test)]
mod tests {
    use crate::schema::doctor::DoctorReport;

    #[test]
    fn doctor_conflict_schema_distinguishes_task_and_install_metadata() {
        let schema =
            super::output_schema::<DoctorReport<'static>>().expect("doctor schema should generate");
        let variants = schema["$defs"]["Conflict"]["oneOf"]
            .as_array()
            .expect("Conflict should be split by kind");
        let variant = |kind: &str| {
            variants
                .iter()
                .find(|variant| variant["properties"]["kind"]["const"] == kind)
                .unwrap_or_else(|| panic!("missing {kind} conflict schema"))
        };

        let task = variant("duplicate-task-name");
        assert_eq!(
            task["properties"]["selected"]["description"],
            "FQN of the winning task."
        );
        assert_eq!(
            task["properties"]["shadowed"]["description"],
            "FQNs of the shadowed tasks."
        );

        let install = variant("install-dir-collision");
        assert_eq!(
            install["properties"]["selected"]["description"],
            "Label of the selected package manager."
        );
        assert_eq!(
            install["properties"]["selector"]["description"],
            "Path of the conflicting installation directory."
        );
        assert_eq!(
            install["properties"]["shadowed"]["description"],
            "Labels of the shadowed package managers."
        );
    }

    #[test]
    fn committed_schemas_match_the_generator() {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas"));
        for document in super::documents().expect("documents should generate") {
            let path = dir.join(document.filename);
            let committed = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            assert_eq!(
                committed,
                format!("{}\n", document.json),
                "{} is stale, run `cargo schema --all --output schemas`",
                document.filename
            );
        }
    }

    #[test]
    fn output_schemas_carry_their_identity() {
        for (schema, command) in [
            (super::output_schema::<DoctorReport<'static>>(), "doctor"),
            (
                super::output_schema::<crate::schema::project::TaskListView<'static>>(),
                "list",
            ),
            (
                super::output_schema::<super::super::why::WhyReport<'static>>(),
                "why",
            ),
        ] {
            let schema = schema.expect("schema should generate");
            assert_eq!(schema["$id"], crate::schema::schema_url(command));
            assert!(
                schema["title"]
                    .as_str()
                    .is_some_and(|title| title.starts_with(&format!("runner {command}"))),
                "{command}: {}",
                schema["title"]
            );
            assert_eq!(
                schema["properties"]["schema_version"]["const"],
                crate::schema::SCHEMA_VERSION
            );
        }
    }
}
