//! `runner schema` — emit committed JSON Schemas (feature `schema`).

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use schemars::{JsonSchema, Schema};
use serde_json::{Map, Value, json};

use crate::schema::{Project, project::TaskListView};

const SCHEMA_DIR: &str = "schemas";

struct SchemaDocument {
    filename: &'static str,
    value: Value,
}

/// Write the config schema to stdout/a file, or every committed schema to a directory.
/// A trailing newline is appended so committed `schemas/*.json` ends cleanly.
pub(crate) fn write_schema(all: bool, output: Option<&Path>) -> Result<()> {
    if all {
        let dir = output.unwrap_or_else(|| Path::new(SCHEMA_DIR));
        write_all_schemas(dir)
    } else {
        write_json(
            output,
            &schema_value(schemars::schema_for!(crate::config::RunnerConfig))?,
        )
    }
}

fn write_all_schemas(dir: &Path) -> Result<()> {
    if dir.exists() && !dir.is_dir() {
        bail!("--all output must be a directory: {}", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    for document in schema_documents()? {
        write_json(Some(&dir.join(document.filename)), &document.value)?;
    }

    Ok(())
}

fn schema_documents() -> Result<Vec<SchemaDocument>> {
    Ok(vec![
        SchemaDocument {
            filename: "runner.toml.schema.json",
            value: schema_value(schemars::schema_for!(crate::config::RunnerConfig))?,
        },
        SchemaDocument {
            filename: "doctor.v1.schema.json",
            value: output_schema::<Project<'static>>("doctor", 1)?,
        },
        SchemaDocument {
            filename: "doctor.v2.schema.json",
            value: output_schema::<Project<'static>>("doctor", 2)?,
        },
        SchemaDocument {
            filename: "list.v1.schema.json",
            value: output_schema::<TaskListView<'static>>("list", 1)?,
        },
        SchemaDocument {
            filename: "list.v2.schema.json",
            value: output_schema::<TaskListView<'static>>("list", 2)?,
        },
        SchemaDocument {
            filename: "why.v1.schema.json",
            value: output_schema::<super::why::WhyReport<'static>>("why", 1)?,
        },
        SchemaDocument {
            filename: "why.v2.schema.json",
            value: output_schema::<super::why::WhyReport<'static>>("why", 2)?,
        },
    ])
}

fn output_schema<T: JsonSchema>(command: &'static str, version: u32) -> Result<Value> {
    let mut schema = serialize_schema_value::<T>()?;
    set_object_field(&mut schema, "$id", json!(schema_id(command, version)));
    set_object_field(&mut schema, "title", json!(title(command, version)));
    set_object_field(
        &mut schema,
        "description",
        json!(description(command, version)),
    );
    patch_schema_version_const(&mut schema, version);
    patch_source_schema(&mut schema, version);
    Ok(schema)
}

fn serialize_schema_value<T: JsonSchema>() -> Result<Value> {
    let generator = schemars::generate::SchemaSettings::default()
        .for_serialize()
        .into_generator();
    schema_value(generator.into_root_schema_for::<T>())
}

fn schema_value(schema: Schema) -> Result<Value> {
    serde_json::to_value(schema).context("failed to serialize schema")
}

fn write_json(output: Option<&Path>, value: &Value) -> Result<()> {
    let mut sorted = value.clone();
    json_schema_sort::sort_schema(&mut sorted);
    let json = serde_json::to_string_pretty(&sorted).context("failed to serialize schema")?;
    output.map_or_else(
        || writeln!(std::io::stdout(), "{json}").context("failed to write schema to stdout"),
        |path| {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("failed to write {}", path.display()))
        },
    )
}

fn set_object_field(schema: &mut Value, key: &'static str, value: Value) {
    if let Some(object) = schema.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

fn patch_schema_version_const(schema: &mut Value, version: u32) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(version_schema) = properties
        .get_mut("schema_version")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    version_schema.insert("const".to_string(), json!(version));
}

fn patch_source_schema(schema: &mut Value, version: u32) {
    let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) else {
        return;
    };

    defs.insert(
        "TaskSourceLabel".to_string(),
        task_source_label_schema(version),
    );
    patch_task_info_source(defs);
    patch_why_candidate_source(defs);
}

fn patch_task_info_source(defs: &mut Map<String, Value>) {
    patch_def_source(defs, "TaskInfo");
}

fn patch_why_candidate_source(defs: &mut Map<String, Value>) {
    patch_def_source(defs, "WhyCandidate");
}

fn patch_def_source(defs: &mut Map<String, Value>, def_name: &'static str) {
    let Some(source_schema) = defs
        .get_mut(def_name)
        .and_then(|definition| definition.get_mut("properties"))
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut("source"))
    else {
        return;
    };
    *source_schema = json!({ "$ref": "#/$defs/TaskSourceLabel" });
}

fn task_source_label_schema(version: u32) -> Value {
    json!({ "type": "string", "enum": source_labels(version) })
}

fn source_labels(version: u32) -> &'static [&'static str] {
    match version {
        1 => &[
            "package.json",
            "Makefile",
            "justfile",
            "Taskfile",
            "turbo.json",
            "deno.json",
            "cargo",
            "go",
            "bacon.toml",
            "mise.toml",
            "pyproject.toml",
        ],
        _ => &[
            "package.json",
            "make",
            "just",
            "task",
            "turbo",
            "deno",
            "cargo",
            "go",
            "bacon",
            "mise",
            "pyproject.toml",
        ],
    }
}

fn schema_id(command: &str, version: u32) -> String {
    format!("https://kjanat.github.io/schemas/{command}.v{version}.schema.json")
}

fn title(command: &str, version: u32) -> String {
    match command {
        "why" => format!("runner why <task> --json --schema-version {version}"),
        _ => format!("runner {command} --json --schema-version {version}"),
    }
}

fn description(command: &str, version: u32) -> String {
    match (command, version) {
        ("doctor", 1) => "JSON schema for the legacy v1 `runner doctor --json` document. v1 uses filename-style task source labels.".to_string(),
        ("doctor", _) => "JSON schema for the current v2 `runner doctor --json` document. v2 uses tool-name task source labels.".to_string(),
        _ => format!("JSON schema for `{}`.", title(command, version)),
    }
}
