//! The generated `runner.toml` JSON Schema, walked by table path, as the
//! single source of truth for hover text and completion.
//!
//! The schema is produced from the `RunnerConfig` doc comments (the same ones
//! the committed `schemas/runner.toml.schema.json` is built from), so editor
//! docs never drift from the struct documentation.

use std::collections::BTreeMap;

use serde_json::Value;

/// One table: its description and its declared fields.
pub(super) struct TableDoc {
    pub description: Option<String>,
    pub fields: BTreeMap<String, FieldDoc>,
}

/// Documentation for one field.
pub(super) struct FieldDoc {
    pub description: Option<String>,
    /// The values the schema lists, each with whether TOML quotes it.
    pub values: Vec<(String, bool)>,
    pub field_type: FieldType,
}

/// The value shape a field's schema declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldType {
    /// A table, writable as its own `[section.field]` header.
    Table,
    /// A string, including closed string enums.
    String,
    /// A boolean.
    Bool,
    /// Anything else.
    Other,
}

/// The schema, indexed by table path.
pub(super) struct SchemaIndex {
    root: &'static Value,
}

impl SchemaIndex {
    pub(super) fn build() -> Self {
        Self {
            root: crate::config::schema(),
        }
    }

    /// The table a dotted header path names: `output.task`, `tasks.build`,
    /// `tasks.build.runtime`. User-named segments go through a map's entry
    /// schema.
    pub(super) fn table(&self, path: &str) -> Option<TableDoc> {
        let mut node = self.resolve(self.root);
        let mut description = None;
        for segment in path.split('.').filter(|segment| !segment.is_empty()) {
            let segment = segment.trim().trim_matches('"');
            let field = node
                .get("properties")
                .and_then(|properties| properties.get(segment))
                .or_else(|| node.get("additionalProperties").filter(|v| v.is_object()))?;
            description = string_field(field, "description");
            node = self.resolve(field);
        }
        let fields = node
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| {
                properties
                    .iter()
                    .map(|(name, field)| (name.clone(), self.field(field)))
                    .collect()
            })
            .unwrap_or_default();
        Some(TableDoc {
            description: description.or_else(|| string_field(node, "description")),
            fields,
        })
    }

    /// Every header path whose tables the schema declares, sorted: each table
    /// field of the root and of its nested tables. Map entries are the user's
    /// names and cannot be listed.
    pub(super) fn header_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        self.collect_paths("", &mut paths);
        paths.sort();
        paths
    }

    fn collect_paths(&self, prefix: &str, paths: &mut Vec<String>) {
        let Some(table) = self.table(prefix) else {
            return;
        };
        for (name, field) in &table.fields {
            if field.field_type == FieldType::Table {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                paths.push(path.clone());
                self.collect_paths(&path, paths);
            }
        }
    }

    fn field(&self, schema: &'static Value) -> FieldDoc {
        let resolved = self.resolve(schema);
        let mut values = Vec::new();
        collect_values(self, schema, &mut values);
        let field_type = if has_type(resolved, "object") || resolved.get("properties").is_some() {
            FieldType::Table
        } else if has_type(resolved, "boolean") {
            FieldType::Bool
        } else if has_type(resolved, "string") || values.iter().any(|(_, quoted)| *quoted) {
            FieldType::String
        } else {
            FieldType::Other
        };
        if field_type == FieldType::Bool && values.is_empty() {
            values = vec![("true".to_owned(), false), ("false".to_owned(), false)];
        }
        FieldDoc {
            description: string_field(schema, "description"),
            values,
            field_type,
        }
    }

    /// `node` with its `$ref` followed, and a nullable `anyOf` narrowed to its
    /// non-null branch.
    fn resolve(&self, node: &'static Value) -> &'static Value {
        if let Some(target) = node
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix('#'))
            .and_then(|pointer| self.root.pointer(pointer))
        {
            return self.resolve(target);
        }
        if let Some([branch]) = node
            .get("anyOf")
            .and_then(Value::as_array)
            .map(|branches| {
                branches
                    .iter()
                    .filter(|branch| branch.get("type").and_then(Value::as_str) != Some("null"))
                    .collect::<Vec<_>>()
            })
            .as_deref()
        {
            return self.resolve(branch);
        }
        node
    }
}

/// The values `schema` lists in `enum`, `const` and `oneOf`/`anyOf` branches.
fn collect_values(index: &SchemaIndex, schema: &'static Value, out: &mut Vec<(String, bool)>) {
    let schema = index.resolve(schema);
    let mut push = |value: &Value| match value {
        Value::String(s) => out.push((s.clone(), true)),
        Value::Bool(b) => out.push((b.to_string(), false)),
        _ => {}
    };
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        values.iter().for_each(&mut push);
    }
    if let Some(value) = schema.get("const") {
        push(value);
    }
    for key in ["oneOf", "anyOf"] {
        for branch in schema
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect_values(index, branch, out);
        }
    }
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

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{FieldType, SchemaIndex};

    #[test]
    fn nested_and_map_tables_resolve() {
        let schema = SchemaIndex::build();
        let task = schema.table("output.task").expect("output.task");
        assert!(task.fields.contains_key("stderr"));
        let build = schema.table("tasks.build").expect("a task entry");
        assert!(build.fields.contains_key("pm"));
        let runtime = schema.table("tasks.build.runtime").expect("its runtime");
        assert_eq!(runtime.fields["javascript"].field_type, FieldType::String);
        assert!(schema.table("zoot").is_none());
    }

    #[test]
    fn values_come_from_enums_consts_and_booleans() {
        let schema = SchemaIndex::build();
        let root = schema.table("").expect("root");
        let download: Vec<&str> = root.fields["download"]
            .values
            .iter()
            .map(|(value, _)| value.as_str())
            .collect();
        assert_eq!(download, ["true", "false", "ask"]);
        let chain = schema.table("chain").expect("chain");
        let actions: Vec<&str> = chain.fields["on_fail"]
            .values
            .iter()
            .map(|(value, _)| value.as_str())
            .collect();
        assert_eq!(actions, ["continue", "wait", "kill"]);
        assert_eq!(
            schema.table("install").unwrap().fields["frozen"].field_type,
            FieldType::Bool
        );
    }

    #[test]
    fn header_paths_reach_nested_tables() {
        let paths = SchemaIndex::build().header_paths();
        for expected in [
            "output",
            "output.parallel",
            "output.task",
            "output.tool",
            "tasks",
        ] {
            assert!(
                paths.iter().any(|path| path == expected),
                "{expected}: {paths:?}"
            );
        }
    }
}
