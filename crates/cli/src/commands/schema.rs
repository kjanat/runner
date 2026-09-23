//! `runner schema`, emit committed JSON Schemas (feature `schema`).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use schemars::{JsonSchema, Schema};
use serde_json::{Map, Value, json};

use crate::schema::project::TaskListView;

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
/// managers, source names, see `resolver::policies::resolve_source_label`),
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
        overrides_values.insert("enum".to_string(), labels.clone());
    }

    // The new per-task `runner` pin (a `[tasks.<name>]` table field) carries the
    // same vocabulary as a legacy `overrides` entry, plus `null` for the
    // `Option`, so a typo like `runner = "trubo"` is flagged in-editor instead
    // of only at runtime by `resolve_source_label`.
    if let Some(runner) = defs
        .get_mut("TaskSettings")
        .and_then(|def| def.get_mut("properties"))
        .and_then(|props| props.get_mut("runner"))
        .and_then(Value::as_object_mut)
    {
        let mut with_null: Vec<Value> = crate::types::task_source_labels()
            .into_iter()
            .map(Value::from)
            .collect();
        with_null.push(Value::Null);
        runner.insert("enum".to_string(), Value::Array(with_null));
    }
    // The bare-string shorthand of a task entry (`build = "turbo"`) is the same
    // pin vocabulary (never null).
    if let Some(spec) = defs.get_mut("TaskSpec") {
        set_string_branch_enum(spec, labels);
    }
    // The bare-string shorthand `verbosity = "quiet"` mirrors the four levels
    // `VerbosityTable.level` already constrains (never null).
    if let Some(config) = defs.get_mut("VerbosityConfig") {
        set_string_branch_enum(config, json!(["off", "quiet", "very-quiet", "silent"]));
    }
}

/// Inject `enum` into the `type: "string"` member of an untagged enum's
/// `anyOf` — the string-shorthand branch of a string-or-table config value
/// ([`crate::config::TaskSpec`], [`crate::config::VerbosityConfig`]) — so the
/// shorthand is validated against the same closed vocabulary as its table form.
fn set_string_branch_enum(def: &mut Value, values: Value) {
    let Some(branches) = def.get_mut("anyOf").and_then(Value::as_array_mut) else {
        return;
    };
    for branch in branches {
        if branch.get("type").and_then(Value::as_str) == Some("string")
            && let Some(obj) = branch.as_object_mut()
        {
            obj.insert("enum".to_string(), values);
            return;
        }
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

fn schema_documents() -> Result<Vec<SchemaDocument>> {
    Ok(vec![
        SchemaDocument {
            filename: "runner.toml.schema.json",
            value: config_schema()?,
        },
        SchemaDocument {
            filename: "doctor.schema.json",
            value: output_schema::<crate::schema::doctor::DoctorReport<'static>>("doctor")?,
        },
        SchemaDocument {
            filename: "list.schema.json",
            value: output_schema::<TaskListView<'static>>("list")?,
        },
        SchemaDocument {
            filename: "why.schema.json",
            value: output_schema::<super::why::WhyReport<'static>>("why")?,
        },
    ])
}

fn output_schema<T: JsonSchema>(command: &'static str) -> Result<Value> {
    let mut schema = serialize_schema_value::<T>()?;
    set_object_field(&mut schema, "$id", json!(schema_id(command)));
    set_object_field(&mut schema, "title", json!(title(command)));
    set_object_field(&mut schema, "description", json!(description(command)));
    patch_schema_version_const(&mut schema);
    patch_source_schema(&mut schema, command);
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

fn patch_schema_version_const(schema: &mut Value) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(version_schema) = properties
        .get_mut("schema_version")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    version_schema.insert("const".to_string(), json!(crate::schema::SCHEMA_VERSION));
}

fn patch_source_schema(schema: &mut Value, command: &str) {
    let Some(defs) = schema.get_mut("$defs").and_then(Value::as_object_mut) else {
        return;
    };

    defs.insert(
        "TaskSourceLabel".to_string(),
        task_source_label_schema(command),
    );
    patch_task_info_source(defs);
    patch_why_task(defs);
    patch_def_field(defs, "SourceEntry", "kind", "TaskSourceLabel");
    patch_overrides_source_labels(defs);
}

/// `Overrides.prefer_sources`/`task_source_pins` hold the same structured
/// source labels as `SourceEntry.kind`, command-dependent like it; reuse
/// `TaskSourceLabel` instead of leaving them generic strings. (Every other
/// `Overrides` label field is backed by a real enum and gets its schema
/// constraint straight from `#[derive(schemars::JsonSchema)]` on that enum,
/// no hand-built schema needed there.)
fn patch_overrides_source_labels(defs: &mut Map<String, Value>) {
    if !defs.contains_key("Overrides") {
        return;
    }
    patch_def_array_items(defs, "Overrides", "prefer_sources", "TaskSourceLabel");
    patch_def_map_array_items(defs, "Overrides", "task_source_pins", "TaskSourceLabel");
}

fn patch_task_info_source(defs: &mut Map<String, Value>) {
    patch_def_field(defs, "TaskInfo", "source", "TaskSourceLabel");
}

/// The `why` task object splits the old flat `source` label into `kind`
/// (mechanism label) and `provider` (executing tool family); constrain
/// both to their closed label sets.
fn patch_why_task(defs: &mut Map<String, Value>) {
    if !defs.contains_key("WhyTask") {
        return;
    }
    defs.insert(
        "ProviderLabel".to_string(),
        json!({ "type": "string", "enum": provider_labels() }),
    );
    patch_def_field(defs, "WhyTask", "kind", "TaskSourceLabel");
    patch_def_field(defs, "WhyTask", "provider", "ProviderLabel");
}

/// Mutable handle on `$defs.<def_name>.properties.<field>`, the shared
/// navigation prefix of every schema patch below.
fn field_schema_mut<'a>(
    defs: &'a mut Map<String, Value>,
    def_name: &str,
    field: &str,
) -> Option<&'a mut Value> {
    defs.get_mut(def_name)
        .and_then(|definition| definition.get_mut("properties"))
        .and_then(Value::as_object_mut)
        .and_then(|properties| properties.get_mut(field))
}

fn def_ref(target_def: &str) -> Value {
    json!({ "$ref": format!("#/$defs/{target_def}") })
}

fn patch_def_field(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    if let Some(field_schema) = field_schema_mut(defs, def_name, field) {
        *field_schema = def_ref(target_def);
    }
}

/// Like [`patch_def_field`], but for an array-typed field, constrains its
/// `items` schema instead of the field itself.
fn patch_def_array_items(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    if let Some(items) = field_schema_mut(defs, def_name, field)
        .and_then(|field_schema| field_schema.get_mut("items"))
    {
        *items = def_ref(target_def);
    }
}

/// Like [`patch_def_array_items`], but for a map-of-array field, constrains
/// the array items nested under `additionalProperties`.
fn patch_def_map_array_items(
    defs: &mut Map<String, Value>,
    def_name: &'static str,
    field: &'static str,
    target_def: &'static str,
) {
    if let Some(items) = field_schema_mut(defs, def_name, field)
        .and_then(|field_schema| field_schema.get_mut("additionalProperties"))
        .and_then(|additional| additional.get_mut("items"))
    {
        *items = def_ref(target_def);
    }
}

fn task_source_label_schema(command: &str) -> Value {
    json!({ "type": "string", "enum": source_labels(command) })
}

/// Closed set for the `why` `provider` field, the tool family that
/// executes the task. Derived from [`crate::types::TaskSource::all`]
/// through [`super::why::provider_label`], the same function `why` calls
/// at runtime, so the committed schema's enum can't drift from it.
fn provider_labels() -> Vec<&'static str> {
    crate::types::TaskSource::all()
        .iter()
        .map(|&source| super::why::provider_label(source))
        .collect()
}

/// Closed label set for `command`'s source labels, derived from
/// [`crate::types::TaskSource::all`] through the same label functions
/// `list`/`doctor`/`why` use at runtime, so the committed schema's enum
/// can't drift from what the binary actually emits. `list` uses the flat
/// label convention; `doctor`/`why` use the structured one (only
/// `CargoAliases` differs, `"cargo-alias"` vs `"cargo"`).
fn source_labels(command: &str) -> Vec<&'static str> {
    use crate::schema::labels::{flat_source_label, structured_source_label};
    use crate::types::TaskSource;

    TaskSource::all()
        .iter()
        .map(|&source| {
            if command == "list" {
                flat_source_label(source)
            } else {
                structured_source_label(source)
            }
        })
        .collect()
}

fn schema_id(command: &str) -> String {
    crate::schema::schema_url(command)
}

fn title(command: &str) -> String {
    match command {
        "why" => "runner why <task> --json".to_string(),
        _ => format!("runner {command} --json"),
    }
}

fn description(command: &str) -> String {
    match command {
        "doctor" => "JSON schema for `runner doctor --json`: structured diagnostic inventory with \
                     invocation/environment provenance, per-ecosystem decisions, sources, \
                     fqn-keyed tasks, tools, conflicts, and diagnostics."
            .to_string(),
        "why" => "JSON schema for `runner why <task> --json`: candidate `{task, match}` pairs \
                  plus the selection decision."
            .to_string(),
        _ => format!("JSON schema for `{}`.", title(command)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    #[test]
    fn doctor_conflict_schema_distinguishes_task_and_install_metadata() {
        let schema = super::output_schema::<crate::schema::doctor::DoctorReport<'static>>("doctor")
            .expect("doctor schema should generate");
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
    fn committed_doctor_example_includes_quiet_override() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/doctor.example.json"
        ))
        .expect("committed doctor example should be readable");
        let example: Value = serde_json::from_str(&raw).expect("example should parse as JSON");

        assert_eq!(example["overrides"]["quiet"], serde_json::json!(false));
        let required =
            super::output_schema::<crate::schema::doctor::DoctorReport<'static>>("doctor")
                .expect("doctor schema should generate")["$defs"]["Overrides"]["required"]
                .as_array()
                .expect("Overrides.required should be an array")
                .clone();
        for field in required {
            let field = field.as_str().expect("required field should be a string");
            assert!(
                example["overrides"].get(field).is_some(),
                "committed doctor example misses required overrides.{field}"
            );
        }
    }

    /// Every committed schema file that carries a `TaskSourceLabel` def,
    /// paired with whether its source labels follow the structured
    /// (`doctor`/`why`) or flat (`list`) convention.
    const COMMITTED_SCHEMAS_WITH_TASK_SOURCE_LABEL: &[(&str, &str)] = &[
        (
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../schemas/doctor.schema.json"
            ),
            "doctor",
        ),
        (
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../schemas/list.schema.json"
            ),
            "list",
        ),
        (
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/why.schema.json"),
            "why",
        ),
    ];

    fn runtime_labels(command: &str) -> Vec<&'static str> {
        use crate::schema::labels::{flat_source_label, structured_source_label};
        use crate::types::TaskSource;

        TaskSource::all()
            .iter()
            .map(|&source| {
                if command == "list" {
                    flat_source_label(source)
                } else {
                    structured_source_label(source)
                }
            })
            .collect()
    }

    #[test]
    fn task_source_label_schema_matches_runtime_labels_per_command() {
        // source_labels(command) used to be three hand-maintained arrays,
        // free to drift from the label functions `list`/`why`/`doctor`
        // actually call at runtime. Now that it's derived, this test is a
        // tautology against today's implementation; its job is to catch a
        // future regression back to a hardcoded list.
        for command in ["list", "doctor", "why"] {
            let schema = super::task_source_label_schema(command);
            let enum_values: Vec<&str> = schema["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{command}: expected enum array"))
                .iter()
                .map(|v| v.as_str().expect("enum values should be strings"))
                .collect();
            assert_eq!(
                enum_values,
                runtime_labels(command),
                "{command}: schema TaskSourceLabel enum must match the runtime label function"
            );
        }
    }

    #[test]
    fn committed_schemas_task_source_label_matches_runtime_labels() {
        // The in-memory generator test above proves the generator is
        // correct; it says nothing about whether `just gen-schema` was
        // actually re-run before committing. Read every checked-in schema
        // that defines TaskSourceLabel directly off disk so a stale
        // artifact, the generator fixed, the commit forgotten, fails
        // here instead of shipping silently.
        for &(path, command) in COMMITTED_SCHEMAS_WITH_TASK_SOURCE_LABEL {
            let raw = std::fs::read_to_string(path)
                .unwrap_or_else(|err| panic!("{path}: should be readable: {err}"));
            let schema: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|err| panic!("{path}: should parse as JSON: {err}"));
            let enum_values: Vec<&str> = schema["$defs"]["TaskSourceLabel"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{path}: expected $defs.TaskSourceLabel.enum array"))
                .iter()
                .map(|v| v.as_str().expect("enum values should be strings"))
                .collect();
            assert_eq!(
                enum_values,
                runtime_labels(command),
                "{path}: committed TaskSourceLabel enum has drifted from the runtime label \
                 function, run `just gen-schema` and commit the result"
            );
        }
    }

    #[test]
    fn provider_label_schema_matches_runtime_labels() {
        // provider_labels() used to be a hand-maintained PROVIDER_LABELS
        // array, free to drift from commands::why::provider_label. Now that
        // it's derived, this test is a tautology against today's
        // implementation; its job is to catch a future regression back
        // to a hardcoded list.
        let enum_values = super::provider_labels();
        let runtime_values: Vec<&str> = crate::types::TaskSource::all()
            .iter()
            .map(|&source| super::super::why::provider_label(source))
            .collect();
        assert_eq!(
            enum_values, runtime_values,
            "ProviderLabel enum must match commands::why::provider_label exactly"
        );
    }

    #[test]
    fn committed_why_schema_provider_label_matches_runtime_labels() {
        // Mirrors committed_schemas_task_source_label_matches_runtime_labels:
        // proves the committed schemas/why.schema.json wasn't left stale
        // after a generator fix.
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/why.schema.json"
        ))
        .expect("committed why schema should be readable");
        let schema: Value = serde_json::from_str(&raw).expect("schema should parse as JSON");
        let enum_values: Vec<&str> = schema["$defs"]["ProviderLabel"]["enum"]
            .as_array()
            .expect("expected $defs.ProviderLabel.enum array")
            .iter()
            .map(|v| v.as_str().expect("enum values should be strings"))
            .collect();
        assert_eq!(
            enum_values,
            super::provider_labels(),
            "schemas/why.schema.json: committed ProviderLabel enum has drifted from \
             commands::why::provider_label, run `just gen-schema` and commit the result"
        );
    }
}
