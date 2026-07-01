//! A flattened view of the generated `runner.toml` JSON Schema, used as the
//! single source of truth for hover text and completion documentation.
//!
//! The schema is produced from the `RunnerConfig` doc comments (the same ones
//! the committed `schemas/runner.toml.schema.json` is built from), so editor
//! docs never drift from the struct documentation.

use std::collections::BTreeMap;

use serde_json::Value;

/// Documentation for one `[section]` and its fields.
pub(super) struct SectionDoc {
    /// The section's own description (from the field that holds it).
    pub description: Option<String>,
    /// Per-field documentation, keyed by field name.
    pub fields: BTreeMap<String, FieldDoc>,
}

/// Documentation for one field within a section.
pub(super) struct FieldDoc {
    /// The field's description, if the schema carries one.
    pub description: Option<String>,
    /// Closed value set (`enum`) the schema declares, if any.
    pub enum_values: Vec<String>,
    /// Whether the schema flags the field as deprecated.
    pub deprecated: bool,
    /// Whether the schema types this field as an object (a nested table
    /// writable as its own `[section.field]` header, e.g. `[tasks.overrides]`).
    pub is_table: bool,
}

/// Section docs keyed by TOML section name (`pm`, `tasks`, …).
pub(super) struct SchemaIndex {
    sections: BTreeMap<String, SectionDoc>,
}

impl SchemaIndex {
    /// Build the index from the crate's generated config schema. A schema that
    /// fails to generate (should not happen) yields an empty index, degrading
    /// hover/completion to no-ops rather than failing the server.
    pub(super) fn build() -> Self {
        let schema = crate::cmd::config_schema().unwrap_or(Value::Null);
        let defs = schema.get("$defs");
        let mut sections = BTreeMap::new();

        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (name, section) in props {
                let description = string_field(section, "description");
                let fields = section
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|r| r.strip_prefix("#/$defs/"))
                    .and_then(|def_name| defs.and_then(|d| d.get(def_name)))
                    .map(field_docs)
                    .unwrap_or_default();
                sections.insert(
                    name.clone(),
                    SectionDoc {
                        description,
                        fields,
                    },
                );
            }
        }
        Self { sections }
    }

    /// Look up a section by its TOML name.
    pub(super) fn section(&self, name: &str) -> Option<&SectionDoc> {
        self.sections.get(name)
    }

    /// Every header path a `[...]` line can name: top-level section names plus
    /// `section.field` for each object-typed field (a nested table, e.g.
    /// `tasks.overrides`), sorted.
    pub(super) fn header_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        for (name, doc) in &self.sections {
            paths.push(name.clone());
            for (field, field_doc) in &doc.fields {
                if field_doc.is_table {
                    paths.push(format!("{name}.{field}"));
                }
            }
        }
        paths.sort();
        paths
    }
}

/// Extract the `properties` of a `$defs` struct schema into per-field docs.
fn field_docs(def: &Value) -> BTreeMap<String, FieldDoc> {
    let Some(props) = def.get("properties").and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    props
        .iter()
        .map(|(field, schema)| {
            let enum_values = schema
                .get("enum")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (
                field.clone(),
                FieldDoc {
                    description: string_field(schema, "description"),
                    enum_values,
                    deprecated: schema.get("deprecated").and_then(Value::as_bool) == Some(true),
                    is_table: is_object_type(schema),
                },
            )
        })
        .collect()
}

/// Whether a field schema declares (possibly among other types, for an
/// `Option<T>`) the JSON `"object"` type.
fn is_object_type(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(s)) => s == "object",
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some("object")),
        _ => false,
    }
}

/// Read a string field from a JSON object, if present.
fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
