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
    /// Whether the schema flags the section as deprecated.
    pub deprecated: bool,
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
    /// The value shape the schema declares.
    pub field_type: FieldType,
}

/// The (single) JSON type shape a field schema declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldType {
    /// An object — a nested table writable as its own `[section.field]`
    /// header, e.g. `[tasks.overrides]`.
    Table,
    /// An array (a TOML sequence, e.g. `prefer = [...]`).
    Array,
    /// A string, including closed string enums.
    String,
    /// Anything else (booleans, numbers).
    Other,
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
                let def = section
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|r| r.strip_prefix("#/$defs/"))
                    .and_then(|def_name| defs.and_then(|d| d.get(def_name)));
                let fields = def.map(field_docs).unwrap_or_default();
                // `deprecated` may sit on the property or (via `extend`) on
                // the referenced `$defs` entry.
                let deprecated = [Some(section), def]
                    .into_iter()
                    .flatten()
                    .any(is_deprecated);
                sections.insert(
                    name.clone(),
                    SectionDoc {
                        description,
                        deprecated,
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
                if field_doc.field_type == FieldType::Table {
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
            let enum_values: Vec<String> = schema
                .get("enum")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let field_type = if has_type(schema, "object") {
                FieldType::Table
            } else if has_type(schema, "array") {
                FieldType::Array
            } else if has_type(schema, "string") || !enum_values.is_empty() {
                FieldType::String
            } else {
                FieldType::Other
            };
            (
                field.clone(),
                FieldDoc {
                    description: string_field(schema, "description"),
                    enum_values,
                    deprecated: is_deprecated(schema),
                    field_type,
                },
            )
        })
        .collect()
}

/// Whether a field schema declares (possibly among other types, for an
/// `Option<T>`) the given JSON type.
fn has_type(schema: &Value, wanted: &str) -> bool {
    match schema.get("type") {
        Some(Value::String(s)) => s == wanted,
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some(wanted)),
        _ => false,
    }
}

/// Whether a schema node carries `"deprecated": true`.
fn is_deprecated(schema: &Value) -> bool {
    schema.get("deprecated").and_then(Value::as_bool) == Some(true)
}

/// Read a string field from a JSON object, if present.
fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}
