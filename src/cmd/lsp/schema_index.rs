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

    /// Every section name, sorted, for top-level completion.
    pub(super) fn section_names(&self) -> impl Iterator<Item = &str> {
        self.sections.keys().map(String::as_str)
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
                },
            )
        })
        .collect()
}

/// Read a string field from a JSON object, if present.
fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
