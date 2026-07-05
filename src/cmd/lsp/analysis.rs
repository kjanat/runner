//! Position analysis for hover and completion.
//!
//! A small, TOML-aware (not TOML-complete) reading of the line under the cursor
//! plus the nearest `[section]` header above it. Enough to answer "what section
//! am I in, and am I on a key or a value?" — which drives both hover lookups and
//! completion candidate sets without a full document parse.

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemTag, Documentation, Hover, HoverContents,
    MarkupContent, MarkupKind, Position,
};

use super::schema_index::SchemaIndex;
use super::text::LineIndex;
use crate::types::{PackageManager, TaskRunner, TaskSource};

/// What the cursor is sitting on within its line.
enum LineShape {
    /// A `[section]` header line; the string is the (possibly partial) path.
    Header(String),
    /// The key side of an assignment (or a bare word being typed as a key).
    Key,
    /// The value side, right of `=`.
    Value {
        /// The key on the left of the `=`.
        key: String,
        /// Whether the cursor sits inside an unclosed `[` array literal.
        in_array: bool,
    },
    /// Blank / whitespace-only line.
    Empty,
}

/// The cursor's section context plus what it's on.
struct Cursor {
    /// Nearest `[section]` header above the cursor line.
    section: Option<String>,
    /// Shape of the cursor's own line.
    shape: LineShape,
}

/// Strip a `[section]` header line to its inner path. Tolerates a missing
/// closing bracket so a half-typed `[ta` still reads as a header.
fn header_path(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?;
    Some(inner.strip_suffix(']').unwrap_or(inner).trim().to_string())
}

/// Read the cursor context from the document text and position.
fn analyze(index: &LineIndex, text: &str, pos: Position) -> Cursor {
    let line_no = pos.line as usize;
    let line_text = text.lines().nth(line_no).unwrap_or("");

    let section = text.lines().take(line_no).filter_map(header_path).last();

    if header_path(line_text).is_some() && line_text.trim_start().starts_with('[') {
        let partial = line_text
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim()
            .to_string();
        return Cursor {
            section,
            shape: LineShape::Header(partial),
        };
    }

    let shape = line_text.find('=').map_or_else(
        || {
            if line_text.trim().is_empty() {
                LineShape::Empty
            } else {
                LineShape::Key
            }
        },
        |eq| {
            let line_start = index.offset(text, Position::new(pos.line, 0));
            let within = index.offset(text, pos).saturating_sub(line_start);
            if within > eq {
                let before_cursor = line_text
                    .get(eq + 1..within.min(line_text.len()))
                    .unwrap_or("");
                LineShape::Value {
                    key: line_text[..eq].trim().to_string(),
                    in_array: before_cursor.matches('[').count()
                        > before_cursor.matches(']').count(),
                }
            } else {
                LineShape::Key
            }
        },
    );

    Cursor { section, shape }
}

/// Build a hover response for the cursor, if it lands on something documented.
pub(super) fn hover(
    index: &LineIndex,
    schema: &SchemaIndex,
    text: &str,
    pos: Position,
) -> Option<Hover> {
    let cursor = analyze(index, text, pos);
    let (title, body) = match cursor.shape {
        LineShape::Header(path) => describe_section(schema, &path)?,
        LineShape::Key | LineShape::Value { .. } => {
            let key = match &cursor.shape {
                LineShape::Value { key, .. } => key.clone(),
                _ => current_key(text, pos)?,
            };
            describe_field(schema, cursor.section.as_deref()?, &key)?
        }
        LineShape::Empty => return None,
    };
    Some(Hover {
        contents: HoverContents::Markup(markdown(&title, &body)),
        range: None,
    })
}

/// The bare key token on the cursor's line (text before `=`, or the first word).
fn current_key(text: &str, pos: Position) -> Option<String> {
    let line = text.lines().nth(pos.line as usize)?;
    let lhs = line.split('=').next().unwrap_or(line).trim();
    let key = lhs.split_whitespace().next()?;
    (!key.is_empty()).then(|| key.to_string())
}

/// Hover/title for a `[section]` (or `[parent.child]` sub-table).
fn describe_section(schema: &SchemaIndex, path: &str) -> Option<(String, String)> {
    if let Some((parent, field)) = path.split_once('.') {
        let doc = schema.section(parent)?.fields.get(field)?;
        return Some((
            format!("[{path}]"),
            deprecation_note(doc.deprecated, doc.description.clone().unwrap_or_default()),
        ));
    }
    let section = schema.section(path)?;
    Some((
        format!("[{path}]"),
        deprecation_note(section.deprecated, section.description.clone()?),
    ))
}

/// Prefix a hover body with a deprecation banner when applicable.
fn deprecation_note(deprecated: bool, body: String) -> String {
    if deprecated {
        format!("**Deprecated.**\n\n{body}")
    } else {
        body
    }
}

/// Hover/title for a `key` within `section`.
fn describe_field(schema: &SchemaIndex, section: &str, key: &str) -> Option<(String, String)> {
    if let Some((parent, sub)) = section.split_once('.') {
        // A sub-table entry (e.g. a pin under `[tasks.overrides]`): describe the
        // owning field, since individual entry keys are user-chosen task names.
        let doc = schema.section(parent)?.fields.get(sub)?;
        return Some((
            format!("[{section}].{key}"),
            doc.description.clone().unwrap_or_default(),
        ));
    }
    let doc = schema.section(section)?.fields.get(key)?;
    let body = deprecation_note(doc.deprecated, doc.description.clone().unwrap_or_default());
    Some((format!("[{section}].{key}"), body))
}

/// Completion candidates for the cursor.
pub(super) fn completion(
    index: &LineIndex,
    schema: &SchemaIndex,
    text: &str,
    pos: Position,
) -> Vec<CompletionItem> {
    let cursor = analyze(index, text, pos);
    match cursor.shape {
        LineShape::Header(partial) => header_items(schema, &partial, false),
        LineShape::Value { key, in_array } => {
            value_items(schema, cursor.section.as_deref(), &key, in_array)
        }
        LineShape::Key => field_items(schema, cursor.section.as_deref()),
        LineShape::Empty => cursor.section.as_deref().map_or_else(
            || header_items(schema, "", true),
            |section| field_items(schema, Some(section)),
        ),
    }
}

/// Header-path completion. A dotted partial (`[tasks.`) completes only the
/// parent's sub-tables (as their child name); an undotted one completes the
/// full top-level list.
fn header_items(schema: &SchemaIndex, partial: &str, bracketed: bool) -> Vec<CompletionItem> {
    match partial.rsplit_once('.') {
        Some((parent, _)) => subtable_items(schema, parent),
        None => section_items(schema, bracketed),
    }
}

/// Section-name completion. `bracketed` wraps the insert text in `[ ]` (for an
/// empty line); otherwise just the name (the `[` is already typed).
fn section_items(schema: &SchemaIndex, bracketed: bool) -> Vec<CompletionItem> {
    schema
        .header_paths()
        .into_iter()
        .map(|name| {
            let insert = if bracketed {
                format!("[{name}]")
            } else {
                name.clone()
            };
            let deprecated = name
                .split('.')
                .next()
                .and_then(|s| schema.section(s))
                .is_some_and(|s| s.deprecated);
            CompletionItem {
                insert_text: Some(insert),
                ..section_item(schema, &name, name.clone(), deprecated)
            }
        })
        .collect()
}

/// Sub-table completion under `parent` (e.g. `overrides` for `[tasks.`).
/// A parent with no sub-tables completes nothing — the top-level list would
/// only mint invalid `[parent.section]` paths.
fn subtable_items(schema: &SchemaIndex, parent: &str) -> Vec<CompletionItem> {
    let prefix = format!("{parent}.");
    schema
        .header_paths()
        .into_iter()
        .filter_map(|path| {
            let child = path.strip_prefix(&prefix)?;
            (!child.contains('.')).then(|| (path.clone(), child.to_string()))
        })
        .map(|(path, child)| {
            let deprecated = schema
                .section(&path)
                .or_else(|| schema.section(parent))
                .is_some_and(|s| s.deprecated);
            section_item(schema, &path, child, deprecated)
        })
        .collect()
}

/// A single section/sub-table completion item labeled `label`, documented
/// from the full `path`.
fn section_item(
    schema: &SchemaIndex,
    path: &str,
    label: String,
    deprecated: bool,
) -> CompletionItem {
    let doc = describe_section(schema, path)
        .map(|(_, body)| body)
        .filter(|body| !body.is_empty())
        .map(doc_markup);
    CompletionItem {
        insert_text: Some(label.clone()),
        label,
        kind: Some(CompletionItemKind::MODULE),
        detail: deprecated.then(|| "deprecated".to_string()),
        tags: deprecated.then(|| vec![CompletionItemTag::DEPRECATED]),
        documentation: doc,
        ..CompletionItem::default()
    }
}

/// Field-name completion for a section.
fn field_items(schema: &SchemaIndex, section: Option<&str>) -> Vec<CompletionItem> {
    let Some(doc) = section.and_then(|s| schema.section(s)) else {
        return Vec::new();
    };
    doc.fields
        .iter()
        .map(|(name, field)| CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            insert_text: Some(format!("{name} = ")),
            detail: field.deprecated.then(|| "deprecated".to_string()),
            tags: field
                .deprecated
                .then(|| vec![CompletionItemTag::DEPRECATED]),
            documentation: field.description.clone().map(doc_markup),
            ..CompletionItem::default()
        })
        .collect()
}

/// Value completion for `section.key`: the schema's `enum`, or a code-driven set
/// for the fields the schema can't enumerate (label lists, booleans).
fn value_items(
    schema: &SchemaIndex,
    section: Option<&str>,
    key: &str,
    in_array: bool,
) -> Vec<CompletionItem> {
    let section = section.unwrap_or("");
    let field = schema.section(section).and_then(|s| s.fields.get(key));
    // For a sequence-typed field with no `[` typed yet, wrap the first
    // element so accepting a completion yields valid TOML.
    let wrap = !in_array && field.is_some_and(|f| f.is_array);
    if let Some(field) = field
        && !field.enum_values.is_empty()
    {
        return field
            .enum_values
            .iter()
            .map(|v| value_item(v, "value", true, wrap))
            .collect();
    }
    code_values(section, key)
        .into_iter()
        .map(|(value, detail)| value_item(&value, detail, detail != "bool", wrap))
        .collect()
}

/// Code-driven value sets for fields the JSON Schema leaves open.
fn code_values(section: &str, key: &str) -> Vec<(String, &'static str)> {
    let label_vocab = || -> Vec<(String, &'static str)> {
        let mut out: Vec<(String, &'static str)> = Vec::new();
        let mut push = |value: String, detail: &'static str| {
            if !out.iter().any(|(v, _)| *v == value) {
                out.push((value, detail));
            }
        };
        for runner in TaskRunner::all() {
            push(runner.label().to_string(), "task runner");
        }
        for pm in PackageManager::all() {
            push(pm.label().to_string(), "package manager");
        }
        for source in TaskSource::all() {
            push(source.label().to_string(), "source");
        }
        out
    };

    match (section, key) {
        ("tasks", "prefer") | ("tasks.overrides", _) => label_vocab(),
        ("task_runner", "prefer") => TaskRunner::all()
            .iter()
            .map(|r| (r.label().to_string(), "task runner"))
            .collect(),
        ("install", "pms") => PackageManager::all()
            .iter()
            .map(|pm| (pm.label().to_string(), "package manager"))
            .collect(),
        ("chain", "keep_going" | "kill_on_fail")
        | ("github", "group_output" | "group_parallel")
        | ("parallel", "grouped") => {
            vec![("true".to_string(), "bool"), ("false".to_string(), "bool")]
        }
        _ => Vec::new(),
    }
}

/// A single value completion item. `quote` wraps `insert_text` in `"..."` for
/// string-typed values, so string fields (`pm.node`, `tasks.prefer`, …) insert
/// valid TOML rather than a bare, unquoted word; `wrap` additionally brackets
/// it as a one-element array for sequence-typed fields. The label stays bare.
fn value_item(value: &str, detail: &str, quote: bool, wrap: bool) -> CompletionItem {
    let mut insert_text = if quote {
        format!("\"{value}\"")
    } else {
        value.to_string()
    };
    if wrap {
        insert_text = format!("[{insert_text}]");
    }
    CompletionItem {
        label: value.to_string(),
        kind: Some(CompletionItemKind::VALUE),
        detail: Some(detail.to_string()),
        insert_text: Some(insert_text),
        ..CompletionItem::default()
    }
}

/// A markdown hover block with a code-fenced title and a body.
fn markdown(title: &str, body: &str) -> MarkupContent {
    let value = if body.is_empty() {
        format!("```toml\n{title}\n```")
    } else {
        format!("```toml\n{title}\n```\n\n{body}")
    };
    MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    }
}

/// Wrap a description string as completion-item markdown documentation.
const fn doc_markup(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

#[cfg(test)]
mod tests {
    use lsp_types::Position;

    use super::super::schema_index::SchemaIndex;
    use super::super::text::LineIndex;
    use super::{completion, hover};

    fn labels(items: &[lsp_types::CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn hover_describes_a_section_header() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\n";
        let result = hover(&LineIndex::new(text), &schema, text, Position::new(0, 2));
        assert!(result.is_some(), "expected hover on a [tasks] header");
    }

    #[test]
    fn completion_offers_section_names_after_bracket() {
        let schema = SchemaIndex::build();
        let text = "[\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(0, 1));
        let names = labels(&items);
        assert!(names.contains(&"tasks"), "{names:?}");
        assert!(names.contains(&"pm"), "{names:?}");
    }

    #[test]
    fn completion_offers_field_names_in_a_section() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\n\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 0));
        let names = labels(&items);
        assert!(names.contains(&"prefer"), "{names:?}");
        assert!(names.contains(&"overrides"), "{names:?}");
    }

    #[test]
    fn completion_offers_label_vocab_for_tasks_prefer() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\nprefer = \n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 9));
        let names = labels(&items);
        assert!(names.contains(&"turbo"), "{names:?}");
        assert!(names.contains(&"bun"), "{names:?}");
        assert!(names.contains(&"package.json"), "{names:?}");
    }

    #[test]
    fn completion_offers_schema_enum_for_pm_node() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nnode = \n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 7));
        let names = labels(&items);
        assert!(names.contains(&"bun"), "{names:?}");
        assert!(names.contains(&"pnpm"), "{names:?}");
    }

    #[test]
    fn completion_offers_nested_section_for_tasks_overrides() {
        let schema = SchemaIndex::build();
        let text = "[\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(0, 1));
        let names = labels(&items);
        assert!(names.contains(&"tasks.overrides"), "{names:?}");
    }

    #[test]
    fn array_field_value_completion_wraps_the_first_element() {
        let schema = SchemaIndex::build();
        let text = "[install]\npms = \n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 6));
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("[\"bun\"]"));
    }

    #[test]
    fn array_field_value_completion_inside_brackets_stays_bare() {
        let schema = SchemaIndex::build();
        let text = "[install]\npms = [\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 7));
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("\"bun\""));
    }

    #[test]
    fn dotted_header_completion_offers_only_the_parents_subtables() {
        let schema = SchemaIndex::build();
        let text = "[tasks.\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(0, 7));
        assert_eq!(labels(&items), vec!["overrides"], "{items:?}");
        assert_eq!(items[0].insert_text.as_deref(), Some("overrides"));
    }

    #[test]
    fn dotted_header_without_subtables_completes_nothing() {
        let schema = SchemaIndex::build();
        let text = "[github.\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(0, 8));
        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn deprecated_section_completion_is_tagged() {
        let schema = SchemaIndex::build();
        let text = "[\n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(0, 1));
        let item = items
            .iter()
            .find(|i| i.label == "task_runner")
            .expect("task_runner item");
        assert_eq!(
            item.tags.as_deref(),
            Some(&[lsp_types::CompletionItemTag::DEPRECATED][..]),
            "{item:?}"
        );
    }

    #[test]
    fn string_value_completions_insert_quoted_text() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nnode = \n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 7));
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("\"bun\""));
    }

    #[test]
    fn bool_value_completions_stay_unquoted() {
        let schema = SchemaIndex::build();
        let text = "[chain]\nkeep_going = \n";
        let items = completion(&LineIndex::new(text), &schema, text, Position::new(1, 13));
        let names = labels(&items);
        assert!(names.contains(&"true"), "{names:?}");
        let item = items.iter().find(|i| i.label == "true").expect("true item");
        assert_eq!(item.insert_text.as_deref(), Some("true"));
    }
}
