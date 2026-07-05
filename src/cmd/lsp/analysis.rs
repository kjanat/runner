//! Position analysis for hover and completion.
//!
//! A small, TOML-aware (not TOML-complete) reading of the line under the cursor
//! plus the nearest `[section]` header above it. Enough to answer "what section
//! am I in, and am I on a key or a value?" — which drives both hover lookups and
//! completion candidate sets without a full document parse.

use std::collections::BTreeMap;
use std::path::Path;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemTag, Documentation, Hover, HoverContents,
    InsertTextFormat, MarkupContent, MarkupKind, Position,
};

use super::schema_index::{FieldDoc, FieldType, SchemaIndex};
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
        /// Whether the cursor sits inside an unclosed `"`/`'` string
        /// literal (odd quote count before it).
        in_string: bool,
    },
    /// Blank / whitespace-only line.
    Empty,
    /// The cursor sits at or after a `#` comment start.
    Comment,
}

/// The cursor's section context plus what it's on.
struct Cursor {
    /// Nearest `[section]` header above the cursor line.
    section: Option<String>,
    /// Shape of the cursor's own line.
    shape: LineShape,
}

/// Byte offset of the `#` that starts a comment on `line`, if any — the
/// first `#` outside a `"`/`'` string literal.
fn comment_start(line: &str) -> Option<usize> {
    let (mut in_basic, mut in_literal) = (false, false);
    for (offset, c) in line.char_indices() {
        match c {
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '#' if !in_basic && !in_literal => return Some(offset),
            _ => {}
        }
    }
    None
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

    let line_start = index.offset(text, Position::new(pos.line, 0));
    let within = index.offset(text, pos).saturating_sub(line_start);
    if comment_start(line_text).is_some_and(|hash| within >= hash) {
        return Cursor {
            section,
            shape: LineShape::Comment,
        };
    }

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
            if within > eq {
                let before_cursor = line_text
                    .get(eq + 1..within.min(line_text.len()))
                    .unwrap_or("");
                LineShape::Value {
                    key: line_text[..eq].trim().to_string(),
                    in_array: before_cursor.matches('[').count()
                        > before_cursor.matches(']').count(),
                    in_string: before_cursor.matches('"').count() % 2 == 1
                        || before_cursor.matches('\'').count() % 2 == 1,
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
        LineShape::Empty | LineShape::Comment => return None,
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

/// Completion candidates for the cursor. `project_dir` anchors project-task
/// discovery for `[tasks.overrides]` entry keys; `snippets` enables
/// value-scaffold inserts (`pms = ["$0"]`) when the client supports them.
pub(super) fn completion(
    index: &LineIndex,
    schema: &SchemaIndex,
    text: &str,
    pos: Position,
    project_dir: Option<&Path>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let cursor = analyze(index, text, pos);
    match cursor.shape {
        LineShape::Header(partial) => header_items(schema, &partial, false),
        LineShape::Value {
            key,
            in_array,
            in_string,
        } => value_items(schema, cursor.section.as_deref(), &key, in_array, in_string),
        LineShape::Key => key_items(
            index,
            schema,
            cursor.section.as_deref(),
            text,
            pos,
            project_dir,
            snippets,
        ),
        LineShape::Empty => match cursor.section.as_deref() {
            None => header_items(schema, "", true),
            Some("tasks.overrides") => task_key_items(project_dir, None, snippets),
            Some(section) => field_items(schema, Some(section), None, snippets),
        },
        LineShape::Comment => Vec::new(),
    }
}

/// Completion on the key side of a line. In `[tasks.overrides]` (or after
/// `overrides.` in `[tasks]`) the keys are the project's own task names, so
/// they complete from task discovery over the document's directory; any
/// other dotted key completes nothing — TOML reads it as a key *path*, and
/// no other section has enumerable sub-keys. The typed token is replaced
/// via an explicit text edit so a client can only ever substitute it, never
/// append to it (a stale list left open after a backspace would otherwise
/// paste at its old anchor).
fn key_items(
    index: &LineIndex,
    schema: &SchemaIndex,
    section: Option<&str>,
    text: &str,
    pos: Position,
    project_dir: Option<&Path>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let Some((token, range)) = key_token(index, text, pos) else {
        return match section {
            Some("tasks.overrides") => task_key_items(project_dir, None, snippets),
            _ => field_items(schema, section, None, snippets),
        };
    };
    match (section, token.rsplit_once('.')) {
        (Some("tasks.overrides"), None) => task_key_items(project_dir, Some(range), snippets),
        // `overrides.<task>` as a dotted key inside `[tasks]`: complete the
        // task name after the dot.
        (Some("tasks"), Some(("overrides", partial))) => {
            let after_dot = lsp_types::Range {
                start: Position {
                    line: range.end.line,
                    character: range.end.character
                        - u32::try_from(partial.chars().count()).unwrap_or(0),
                },
                end: range.end,
            };
            task_key_items(project_dir, Some(after_dot), snippets)
        }
        (_, Some(_)) => Vec::new(),
        (_, None) => field_items(schema, section, Some(range), snippets),
    }
}

/// Key completions for `[tasks.overrides]` entries: the project's own task
/// names, discovered from `project_dir` with the same detection the CLI
/// uses. Names that aren't bare TOML keys insert quoted.
fn task_key_items(
    project_dir: Option<&Path>,
    replace: Option<lsp_types::Range>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let Some(dir) = project_dir else {
        return Vec::new();
    };
    // First source wins on duplicate names, matching dispatch display.
    let mut tasks: BTreeMap<String, (&'static str, Option<String>)> = BTreeMap::new();
    for task in crate::detect::detect(dir).tasks {
        tasks
            .entry(task.name)
            .or_insert_with(|| (task.source.label(), task.description));
    }
    tasks
        .into_iter()
        .map(|(name, (source, description))| {
            // An override pin's value is a source-label string.
            let (new_text, format) = if snippets {
                (
                    format!("{} = \"$0\"", toml_key(&name)),
                    Some(InsertTextFormat::SNIPPET),
                )
            } else {
                (format!("{} = ", toml_key(&name)), None)
            };
            CompletionItem {
                text_edit: replace.map(|range| {
                    lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                        range,
                        new_text: new_text.clone(),
                    })
                }),
                insert_text: Some(new_text),
                insert_text_format: format,
                label: name,
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(source.to_string()),
                documentation: description.map(doc_markup),
                ..CompletionItem::default()
            }
        })
        .collect()
}

/// Render a task name as a TOML key: bare when possible, quoted otherwise
/// (e.g. `build:web` → `"build:web"`).
fn toml_key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// The whitespace-delimited token immediately before the cursor and its
/// range, when non-empty.
fn key_token(index: &LineIndex, text: &str, pos: Position) -> Option<(String, lsp_types::Range)> {
    let line_text = text.lines().nth(pos.line as usize)?;
    let line_start = index.offset(text, Position::new(pos.line, 0));
    let within = index
        .offset(text, pos)
        .saturating_sub(line_start)
        .min(line_text.len());
    let before = line_text.get(..within)?;
    let token_start = before.rfind(char::is_whitespace).map_or(0, |i| {
        i + before[i..].chars().next().map_or(1, char::len_utf8)
    });
    let token = &before[token_start..];
    if token.is_empty() {
        return None;
    }
    Some((
        token.to_string(),
        index.range(text, line_start + token_start, line_start + within),
    ))
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

/// Field-name completion for a section. With `replace`, each item carries a
/// text edit substituting the typed token instead of inserting at the cursor.
fn field_items(
    schema: &SchemaIndex,
    section: Option<&str>,
    replace: Option<lsp_types::Range>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let Some(doc) = section.and_then(|s| schema.section(s)) else {
        return Vec::new();
    };
    doc.fields
        .iter()
        .map(|(name, field)| {
            let (new_text, format) = field_insert(name, field, snippets);
            CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                text_edit: replace.map(|range| {
                    lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                        range,
                        new_text: new_text.clone(),
                    })
                }),
                insert_text: Some(new_text),
                insert_text_format: format,
                detail: field.deprecated.then(|| "deprecated".to_string()),
                tags: field
                    .deprecated
                    .then(|| vec![CompletionItemTag::DEPRECATED]),
                documentation: field.description.clone().map(doc_markup),
                ..CompletionItem::default()
            }
        })
        .collect()
}

/// The insert text for a completed field key, scaffolding the value shape
/// its schema type calls for: `[$0]`-style snippets for arrays, `"$0"` for
/// strings, a bare tab stop otherwise. A table-typed field continues as a
/// dotted key path (`overrides.`), which re-triggers completion. Without
/// client snippet support everything falls back to the plain `name = `.
fn field_insert(
    name: &str,
    field: &FieldDoc,
    snippets: bool,
) -> (String, Option<InsertTextFormat>) {
    if field.field_type == FieldType::Table {
        return (format!("{name}."), None);
    }
    if !snippets {
        return (format!("{name} = "), None);
    }
    let scaffold = match field.field_type {
        // Every array field in the schema holds strings.
        FieldType::Array => "[\"$0\"]",
        FieldType::String => "\"$0\"",
        FieldType::Table | FieldType::Other => "$0",
    };
    (
        format!("{name} = {scaffold}"),
        Some(InsertTextFormat::SNIPPET),
    )
}

/// Value completion for `section.key`: the schema's `enum`, or a code-driven set
/// for the fields the schema can't enumerate (label lists, booleans).
fn value_items(
    schema: &SchemaIndex,
    section: Option<&str>,
    key: &str,
    in_array: bool,
    in_string: bool,
) -> Vec<CompletionItem> {
    let section = section.unwrap_or("");
    let field = schema.section(section).and_then(|s| s.fields.get(key));
    // For a sequence-typed field with no `[` typed yet, wrap the first
    // element so accepting a completion yields valid TOML. Inside an open
    // string literal the quotes (and brackets) are already typed — insert
    // the bare word.
    let wrap = !in_array && !in_string && field.is_some_and(|f| f.field_type == FieldType::Array);
    if let Some(field) = field
        && !field.enum_values.is_empty()
    {
        return field
            .enum_values
            .iter()
            .map(|v| value_item(v, "value", !in_string, wrap))
            .collect();
    }
    code_values(section, key)
        .into_iter()
        .map(|(value, detail)| value_item(&value, detail, detail != "bool" && !in_string, wrap))
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
        // `overrides.<task> = ...` as a dotted key inside `[tasks]`.
        ("tasks", key) if key.starts_with("overrides.") => label_vocab(),
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
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(0, 1),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"tasks"), "{names:?}");
        assert!(names.contains(&"pm"), "{names:?}");
    }

    #[test]
    fn completion_offers_field_names_in_a_section() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\n\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 0),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"prefer"), "{names:?}");
        assert!(names.contains(&"overrides"), "{names:?}");
    }

    #[test]
    fn completion_offers_label_vocab_for_tasks_prefer() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\nprefer = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 9),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"turbo"), "{names:?}");
        assert!(names.contains(&"bun"), "{names:?}");
        assert!(names.contains(&"package.json"), "{names:?}");
    }

    #[test]
    fn completion_offers_schema_enum_for_pm_node() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nnode = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 7),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"bun"), "{names:?}");
        assert!(names.contains(&"pnpm"), "{names:?}");
    }

    #[test]
    fn completion_offers_nested_section_for_tasks_overrides() {
        let schema = SchemaIndex::build();
        let text = "[\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(0, 1),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"tasks.overrides"), "{names:?}");
    }

    #[test]
    fn array_field_value_completion_wraps_the_first_element() {
        let schema = SchemaIndex::build();
        let text = "[install]\npms = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 6),
            None,
            false,
        );
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("[\"bun\"]"));
    }

    #[test]
    fn array_field_value_completion_inside_brackets_stays_bare() {
        let schema = SchemaIndex::build();
        let text = "[install]\npms = [\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 7),
            None,
            false,
        );
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("\"bun\""));
    }

    #[test]
    fn dotted_header_completion_offers_only_the_parents_subtables() {
        let schema = SchemaIndex::build();
        let text = "[tasks.\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(0, 7),
            None,
            false,
        );
        assert_eq!(labels(&items), vec!["overrides"], "{items:?}");
        assert_eq!(items[0].insert_text.as_deref(), Some("overrides"));
    }

    #[test]
    fn dotted_header_without_subtables_completes_nothing() {
        let schema = SchemaIndex::build();
        let text = "[github.\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(0, 8),
            None,
            false,
        );
        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn deprecated_section_completion_is_tagged() {
        let schema = SchemaIndex::build();
        let text = "[\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(0, 1),
            None,
            false,
        );
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
    fn dotted_key_completes_nothing() {
        let schema = SchemaIndex::build();
        let text = "[github]\ngroup_output.\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 13),
            None,
            false,
        );
        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn key_completion_replaces_the_typed_token() {
        let schema = SchemaIndex::build();
        let text = "[github]\ngroup_o\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 7),
            None,
            false,
        );
        let item = items
            .iter()
            .find(|i| i.label == "group_output")
            .expect("group_output item");
        let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &item.text_edit else {
            panic!("expected a plain text edit: {item:?}");
        };
        assert_eq!(
            (edit.range.start.character, edit.range.end.character),
            (0, 7),
            "{edit:?}"
        );
        assert_eq!(edit.new_text, "group_output = ");
    }

    #[test]
    fn tasks_overrides_keys_complete_project_task_names() {
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("lsp-overrides-tasks");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build:web": "vite build" } }"#,
        )
        .expect("package.json should be written");

        let schema = SchemaIndex::build();
        let text = "[tasks.overrides]\n\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 0),
            Some(dir.path()),
            false,
        );
        let dev = items.iter().find(|i| i.label == "dev").expect("dev item");
        assert_eq!(dev.insert_text.as_deref(), Some("dev = "));
        // A name that isn't a bare TOML key inserts quoted.
        let web = items
            .iter()
            .find(|i| i.label == "build:web")
            .expect("build:web item");
        assert_eq!(web.insert_text.as_deref(), Some("\"build:web\" = "));
    }

    #[test]
    fn dotted_overrides_key_completes_task_names_after_the_dot() {
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("lsp-overrides-dotted");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "dev": "vite" } }"#,
        )
        .expect("package.json should be written");

        let schema = SchemaIndex::build();
        let text = "[tasks]\noverrides.\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 10),
            Some(dir.path()),
            false,
        );
        let dev = items.iter().find(|i| i.label == "dev").expect("dev item");
        let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &dev.text_edit else {
            panic!("expected a text edit: {dev:?}");
        };
        // Replaces only the (empty) partial after the dot, not `overrides.`.
        assert_eq!(
            (edit.range.start.character, edit.range.end.character),
            (10, 10),
            "{edit:?}"
        );
    }

    #[test]
    fn dotted_overrides_value_completes_source_labels() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\noverrides.dev = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 17),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"just"), "{names:?}");
    }

    #[test]
    fn array_field_key_completion_scaffolds_an_array_snippet() {
        let schema = SchemaIndex::build();
        let text = "[install]\npm\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 2),
            None,
            true,
        );
        let pms = items.iter().find(|i| i.label == "pms").expect("pms item");
        assert_eq!(pms.insert_text.as_deref(), Some("pms = [\"$0\"]"));
        assert_eq!(
            pms.insert_text_format,
            Some(lsp_types::InsertTextFormat::SNIPPET)
        );
    }

    #[test]
    fn string_field_key_completion_scaffolds_a_string_snippet() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nno\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 2),
            None,
            true,
        );
        let node = items.iter().find(|i| i.label == "node").expect("node item");
        assert_eq!(node.insert_text.as_deref(), Some("node = \"$0\""));
    }

    #[test]
    fn bool_field_key_completion_scaffolds_a_bare_tab_stop() {
        let schema = SchemaIndex::build();
        let text = "[github]\ngr\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 2),
            None,
            true,
        );
        let item = items
            .iter()
            .find(|i| i.label == "group_output")
            .expect("group_output item");
        assert_eq!(item.insert_text.as_deref(), Some("group_output = $0"));
    }

    #[test]
    fn table_field_key_completion_continues_the_key_path() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\nov\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 2),
            None,
            true,
        );
        let item = items
            .iter()
            .find(|i| i.label == "overrides")
            .expect("overrides item");
        assert_eq!(item.insert_text.as_deref(), Some("overrides."));
        assert_eq!(item.insert_text_format, None);
    }

    #[test]
    fn without_snippet_support_key_completion_stays_plain() {
        let schema = SchemaIndex::build();
        let text = "[install]\npm\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 2),
            None,
            false,
        );
        let pms = items.iter().find(|i| i.label == "pms").expect("pms item");
        assert_eq!(pms.insert_text.as_deref(), Some("pms = "));
        assert_eq!(pms.insert_text_format, None);
    }

    #[test]
    fn key_token_survives_multibyte_whitespace() {
        let schema = SchemaIndex::build();
        // U+3000 ideographic space (3 bytes) before the key: slicing at
        // whitespace-index + 1 would split the codepoint and panic.
        let text = "[github]\n\u{3000}gr\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 3),
            None,
            false,
        );
        assert!(
            labels(&items).contains(&"group_output"),
            "{:?}",
            labels(&items)
        );
    }

    #[test]
    fn comment_line_completes_nothing() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\n# prefer = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 11),
            None,
            false,
        );
        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn trailing_comment_completes_nothing_but_the_value_before_it_still_does() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nnode =  # pick one\n";
        // After the `#`: nothing.
        let in_comment = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 10),
            None,
            false,
        );
        assert!(in_comment.is_empty(), "{:?}", labels(&in_comment));
        // On the value side, before the `#`: normal value completion.
        let before = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 7),
            None,
            false,
        );
        assert!(labels(&before).contains(&"bun"), "{:?}", labels(&before));
    }

    #[test]
    fn hash_inside_a_string_is_not_a_comment() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\nprefer = [\"#\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 12),
            None,
            false,
        );
        assert!(labels(&items).contains(&"turbo"), "{:?}", labels(&items));
    }

    #[test]
    fn hover_in_a_comment_is_silent() {
        let schema = SchemaIndex::build();
        let text = "[tasks]\n# prefer\n";
        let result = hover(&LineIndex::new(text), &schema, text, Position::new(1, 4));
        assert!(result.is_none(), "{result:?}");
    }

    #[test]
    fn value_completion_inside_an_open_string_stays_bare() {
        let schema = SchemaIndex::build();
        // `prefer = ["ba` — the quote is already typed (and the client may
        // auto-pair the closer); inserting a quoted value would double it.
        let text = "[tasks]\nprefer = [\"ba\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 13),
            None,
            false,
        );
        let bacon = items.iter().find(|i| i.label == "bacon").expect("bacon");
        assert_eq!(bacon.insert_text.as_deref(), Some("bacon"));
    }

    #[test]
    fn scalar_value_completion_inside_an_open_string_stays_bare() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nnode = \"b\n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 9),
            None,
            false,
        );
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("bun"));
    }

    #[test]
    fn string_value_completions_insert_quoted_text() {
        let schema = SchemaIndex::build();
        let text = "[pm]\nnode = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 7),
            None,
            false,
        );
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("\"bun\""));
    }

    #[test]
    fn bool_value_completions_stay_unquoted() {
        let schema = SchemaIndex::build();
        let text = "[chain]\nkeep_going = \n";
        let items = completion(
            &LineIndex::new(text),
            &schema,
            text,
            Position::new(1, 13),
            None,
            false,
        );
        let names = labels(&items);
        assert!(names.contains(&"true"), "{names:?}");
        let item = items.iter().find(|i| i.label == "true").expect("true item");
        assert_eq!(item.insert_text.as_deref(), Some("true"));
    }
}
