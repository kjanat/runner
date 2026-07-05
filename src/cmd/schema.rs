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
        write_json(output, &config_schema()?)
    }
}

/// The `runner.toml` config schema, tagged with its canonical `$id` so the
/// committed file self-identifies (matching the `#:schema` directive the
/// scaffold writes).
pub(crate) fn config_schema() -> Result<Value> {
    let mut schema = schema_value(schemars::schema_for!(crate::config::RunnerConfig))?;
    set_object_field(
        &mut schema,
        "$id",
        json!(crate::schema::config_schema_url()),
    );
    patch_tasks_label_vocab(&mut schema);
    Ok(schema)
}

/// Constrain `[tasks].prefer` and `[tasks.overrides]` values to the closed
/// label vocabulary the resolver actually accepts (task runners, package
/// managers, source names — see `resolver::policies::resolve_source_label`),
/// instead of leaving them as unconstrained strings. Derived from
/// [`crate::types::task_source_labels`] so the schema can't drift from the
/// resolver's own vocabulary.
fn patch_tasks_label_vocab(schema: &mut Value) {
    let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(tasks) = defs
        .get_mut("TasksSection")
        .and_then(|def| def.get_mut("properties"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let labels = json!(crate::types::task_source_labels());
    if let Some(prefer_items) = tasks
        .get_mut("prefer")
        .and_then(|f| f.get_mut("items"))
        .and_then(Value::as_object_mut)
    {
        prefer_items.insert("enum".to_string(), labels.clone());
    }
    if let Some(overrides_values) = tasks
        .get_mut("overrides")
        .and_then(|f| f.get_mut("additionalProperties"))
        .and_then(Value::as_object_mut)
    {
        overrides_values.insert("enum".to_string(), labels);
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
            value: config_schema()?,
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
            filename: "doctor.v3.schema.json",
            value: output_schema::<crate::schema::doctor_v3::DoctorReportV3<'static>>("doctor", 3)?,
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
        SchemaDocument {
            filename: "why.v3.schema.json",
            value: output_schema::<super::why::WhyReportV3<'static>>("why", 3)?,
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
    patch_schema_compat(&mut schema, command, version);
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
    patch_why_task_v3(defs);
    patch_def_field(defs, "SourceV3", "kind", "TaskSourceLabel");
}

fn patch_schema_compat(schema: &mut Value, command: &str, version: u32) {
    if command == "doctor" && version == 3 {
        // v3 existed before `quiet`; keep additive fields optional so the
        // current public schema still validates older v3 payloads.
        remove_required_def_field(schema, "OverridesV3", "quiet");
    }
}

fn remove_required_def_field(schema: &mut Value, def_name: &'static str, field: &'static str) {
    let Some(required) = schema
        .get_mut("$defs")
        .and_then(Value::as_object_mut)
        .and_then(|defs| defs.get_mut(def_name))
        .and_then(|definition| definition.get_mut("required"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    required.retain(|name| name.as_str() != Some(field));
}

fn patch_task_info_source(defs: &mut Map<String, Value>) {
    patch_def_field(defs, "TaskInfo", "source", "TaskSourceLabel");
}

fn patch_why_candidate_source(defs: &mut Map<String, Value>) {
    patch_def_field(defs, "WhyCandidate", "source", "TaskSourceLabel");
}

/// The v3 `why` task object splits the old `source` label into `kind`
/// (mechanism label) and `provider` (executing tool family); constrain
/// both to their closed label sets.
fn patch_why_task_v3(defs: &mut Map<String, Value>) {
    if !defs.contains_key("WhyTaskV3") {
        return;
    }
    defs.insert(
        "ProviderLabel".to_string(),
        json!({ "type": "string", "enum": PROVIDER_LABELS }),
    );
    patch_def_field(defs, "WhyTaskV3", "kind", "TaskSourceLabel");
    patch_def_field(defs, "WhyTaskV3", "provider", "ProviderLabel");
}

fn patch_def_field(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    let Some(field_schema) = defs
        .get_mut(def_name)
        .and_then(|definition| definition.get_mut("properties"))
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut(field))
    else {
        return;
    };
    *field_schema = json!({ "$ref": format!("#/$defs/{target_def}") });
}

fn task_source_label_schema(version: u32) -> Value {
    json!({ "type": "string", "enum": source_labels(version) })
}

/// Closed set for the v3 `provider` field — the tool family that
/// executes the task. Mirrors `cmd::why::provider_label`.
const PROVIDER_LABELS: &[&str] = &[
    "node", "make", "just", "task", "turbo", "deno", "cargo", "go", "bacon", "mise", "python",
];

/// Closed label set for schema version `version`, derived from
/// [`crate::types::TaskSource::all`] through the same
/// [`crate::schema::labels::source_label_for`] dispatcher `why`/`doctor`
/// use at runtime — so the committed schema's enum can't drift from what
/// the binary actually emits.
fn source_labels(version: u32) -> Vec<&'static str> {
    crate::types::TaskSource::all()
        .iter()
        .map(|&source| crate::schema::labels::source_label_for(source, version))
        .collect()
}

fn schema_id(command: &str, version: u32) -> String {
    crate::schema::schema_url(command, version)
}

fn title(command: &str, version: u32) -> String {
    match command {
        "why" => format!("runner why <task> --json --schema-version {version}"),
        _ => format!("runner {command} --json --schema-version {version}"),
    }
}

fn description(command: &str, version: u32) -> String {
    match (command, version) {
        ("doctor", 1) => "JSON schema for the legacy v1 `runner doctor --json` document. v1 uses \
                          filename-style task source labels."
            .to_string(),
        ("doctor", 2) => "JSON schema for the v2 `runner doctor --json` document. v2 uses \
                          tool-name task source labels."
            .to_string(),
        ("doctor", _) => "JSON schema for the current v3 `runner doctor --json` document: \
                          structured diagnostic inventory with invocation/environment provenance, \
                          per-ecosystem decisions, sources, fqn-keyed tasks, tools, conflicts, \
                          and diagnostics."
            .to_string(),
        _ => format!("JSON schema for `{}`.", title(command, version)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::output_schema;

    fn overrides_v3(schema: &Value) -> &Value {
        schema
            .get("$defs")
            .and_then(Value::as_object)
            .and_then(|defs| defs.get("OverridesV3"))
            .expect("schema should define OverridesV3")
    }

    fn quiet_is_optional(schema: &Value) -> bool {
        overrides_v3(schema)
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|required| !required.iter().any(|name| name.as_str() == Some("quiet")))
    }

    fn quiet_type(schema: &Value) -> Option<&str> {
        overrides_v3(schema)
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get("quiet"))
            .and_then(|quiet| quiet.get("type"))
            .and_then(Value::as_str)
    }

    #[test]
    fn doctor_v3_schema_keeps_quiet_optional_for_compat() {
        let schema =
            output_schema::<crate::schema::doctor_v3::DoctorReportV3<'static>>("doctor", 3)
                .expect("doctor v3 schema should render");

        assert!(quiet_is_optional(&schema));
        assert_eq!(quiet_type(&schema), Some("boolean"));
    }

    #[test]
    fn committed_doctor_v3_schema_keeps_quiet_optional_for_compat() {
        let raw = std::fs::read_to_string("schemas/doctor.v3.schema.json")
            .expect("committed doctor v3 schema should be readable");
        let schema: Value = serde_json::from_str(&raw).expect("schema should parse as JSON");

        assert!(quiet_is_optional(&schema));
        assert_eq!(quiet_type(&schema), Some("boolean"));
    }

    #[test]
    fn committed_doctor_v3_example_includes_quiet_override() {
        let raw = std::fs::read_to_string("schemas/doctor.v3.example.json")
            .expect("committed doctor v3 example should be readable");
        let example: Value = serde_json::from_str(&raw).expect("example should parse as JSON");

        assert_eq!(example["overrides"]["quiet"], serde_json::json!(false));
    }

    #[test]
    fn task_source_label_schema_matches_runtime_labels_per_version() {
        // source_labels(version) used to be three hand-maintained arrays,
        // free to drift from the source_label_for dispatcher `why`/`doctor`
        // actually call at runtime. Now that it's derived, this test is a
        // tautology against today's implementation — its job is to catch a
        // future regression back to a hardcoded list.
        use crate::schema::labels::source_label_for;
        use crate::types::TaskSource;

        for version in 1..=3 {
            let schema = super::task_source_label_schema(version);
            let enum_values: Vec<&str> = schema["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("v{version}: expected enum array"))
                .iter()
                .map(|v| v.as_str().expect("enum values should be strings"))
                .collect();
            let runtime_labels: Vec<&str> = TaskSource::all()
                .iter()
                .map(|&source| source_label_for(source, version))
                .collect();
            assert_eq!(
                enum_values, runtime_labels,
                "v{version}: schema TaskSourceLabel enum must match source_label_for exactly"
            );
        }
    }
}
