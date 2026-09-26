//! Position analysis for hover and completion.
//!
//! A small, TOML-aware (not TOML-complete) reading of the line under the cursor
//! plus the nearest `[section]` header above it. Enough to answer "what section
//! am I in, and am I on a key or a value?", which drives both hover lookups and
//! completion candidate sets without a full document parse.

use std::collections::BTreeMap;
use std::path::Path;

use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, Hover, HoverContents, InsertTextFormat,
    MarkupContent, MarkupKind, Position,
};

use super::schema_index::{FieldDoc, FieldType, SchemaIndex, TableDoc};
use super::text::LineIndex;
use crate::provider::Named;

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

/// Byte offset of the `#` that starts a comment on `line`, if any, the
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
            describe_field(schema, cursor.section.as_deref().unwrap_or(""), &key)?
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

/// Hover/title for a `[section]` header.
fn describe_section(schema: &SchemaIndex, path: &str) -> Option<(String, String)> {
    let table = schema.table(path)?;
    Some((format!("[{path}]"), table.description.unwrap_or_default()))
}

/// Hover/title for a `key` (possibly dotted) within `section`.
fn describe_field(schema: &SchemaIndex, section: &str, key: &str) -> Option<(String, String)> {
    let (table, field) = field_of(schema, section, key)?;
    let title = if section.is_empty() {
        key.to_owned()
    } else {
        format!("[{section}].{key}")
    };
    Some((
        title,
        table
            .fields
            .get(&field)
            .and_then(|doc| doc.description.clone())
            .unwrap_or_default(),
    ))
}

/// The table holding the last segment of a dotted `key` in `section`, and that
/// segment.
fn field_of(schema: &SchemaIndex, section: &str, key: &str) -> Option<(TableDoc, String)> {
    let (prefix, field) = key.rsplit_once('.').unwrap_or(("", key));
    let path = [section, prefix]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".");
    Some((schema.table(&path)?, field.trim().to_owned()))
}

/// Completion candidates for the cursor. `project_dir` anchors project-task
/// discovery for `[tasks.<name>]` headers and keys; `snippets` enables
/// value-scaffold inserts (`key = "$0"`) when the client supports them.
pub(super) fn completion(
    index: &LineIndex,
    schema: &SchemaIndex,
    text: &str,
    pos: Position,
    project_dir: Option<&Path>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let cursor = analyze(index, text, pos);
    let section = cursor.section.as_deref().unwrap_or("");
    match cursor.shape {
        LineShape::Header(partial) => header_items(schema, &partial, project_dir, false),
        LineShape::Value { key, in_string } => value_items(schema, section, &key, in_string),
        LineShape::Key => key_items(index, schema, section, text, pos, project_dir, snippets),
        LineShape::Empty if cursor.section.is_none() => {
            let mut items = field_items(schema, "", None, snippets);
            items.extend(header_items(schema, "", project_dir, true));
            items
        }
        LineShape::Empty => entry_items(schema, section, None, project_dir, snippets),
        LineShape::Comment => Vec::new(),
    }
}

/// Completion on the key side of a line. A dotted key completes the fields of
/// the table its path names; the typed token is replaced via an explicit text
/// edit so a client can only ever substitute it, never append to it.
fn key_items(
    index: &LineIndex,
    schema: &SchemaIndex,
    section: &str,
    text: &str,
    pos: Position,
    project_dir: Option<&Path>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let Some((token, range)) = key_token(index, text, pos) else {
        return entry_items(schema, section, None, project_dir, snippets);
    };
    let Some((prefix, partial)) = token.rsplit_once('.') else {
        return entry_items(schema, section, Some(range), project_dir, snippets);
    };
    let after_dot = lsp_types::Range {
        start: Position {
            line: range.end.line,
            character: range.end.character - u32::try_from(partial.chars().count()).unwrap_or(0),
        },
        end: range.end,
    };
    let path = [section, prefix]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".");
    entry_items(schema, &path, Some(after_dot), project_dir, snippets)
}

/// The keys a table takes: its declared fields, or for `[tasks]` the project's
/// own task names.
fn entry_items(
    schema: &SchemaIndex,
    path: &str,
    replace: Option<lsp_types::Range>,
    project_dir: Option<&Path>,
    snippets: bool,
) -> Vec<CompletionItem> {
    if path == "tasks" {
        return task_names(project_dir)
            .into_iter()
            .map(|(name, source, description)| {
                edit_item(
                    name.clone(),
                    format!("{}.", toml_key(&name)),
                    None,
                    replace,
                    Some(source.to_owned()),
                    description,
                )
            })
            .collect();
    }
    field_items(schema, path, replace, snippets)
}

/// The project's task names, discovered from `project_dir` with the detection
/// the CLI uses, first source winning on duplicate names.
fn task_names(project_dir: Option<&Path>) -> Vec<(String, &'static str, Option<String>)> {
    let Some(dir) = project_dir else {
        return Vec::new();
    };
    let mut tasks: BTreeMap<String, (&'static str, Option<String>)> = BTreeMap::new();
    for task in crate::detect::detect(dir, &crate::resolver::ResolutionOverrides::default()).tasks {
        tasks
            .entry(task.name)
            .or_insert_with(|| (task.source.label(), task.description));
    }
    tasks
        .into_iter()
        .map(|(name, (source, description))| (name, source, description))
        .collect()
}

/// A completion item inserting `new_text`, over `replace` when given.
fn edit_item(
    label: String,
    new_text: String,
    format: Option<InsertTextFormat>,
    replace: Option<lsp_types::Range>,
    detail: Option<String>,
    documentation: Option<String>,
) -> CompletionItem {
    CompletionItem {
        text_edit: replace.map(|range| {
            lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                range,
                new_text: new_text.clone(),
            })
        }),
        insert_text: Some(new_text),
        insert_text_format: format,
        label,
        kind: Some(CompletionItemKind::FIELD),
        detail,
        documentation: documentation.map(doc_markup),
        ..CompletionItem::default()
    }
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

/// Header-path completion. A dotted partial (`[output.`) completes only the
/// parent's sub-tables as their child name, and `[tasks.` the project's task
/// names; an undotted one completes every header path the schema declares.
fn header_items(
    schema: &SchemaIndex,
    partial: &str,
    project_dir: Option<&Path>,
    bracketed: bool,
) -> Vec<CompletionItem> {
    let Some((parent, _)) = partial.rsplit_once('.') else {
        return schema
            .header_paths()
            .into_iter()
            .map(|path| {
                let insert = if bracketed {
                    format!("[{path}]")
                } else {
                    path.clone()
                };
                section_item(schema, &path, path.clone(), insert)
            })
            .collect();
    };
    if parent == "tasks" {
        return task_names(project_dir)
            .into_iter()
            .map(|(name, _, _)| {
                let key = toml_key(&name);
                section_item(schema, "tasks", name, key)
            })
            .collect();
    }
    let Some(table) = schema.table(parent) else {
        return Vec::new();
    };
    table
        .fields
        .iter()
        .filter(|(_, field)| field.field_type == FieldType::Table)
        .map(|(name, _)| {
            section_item(
                schema,
                &format!("{parent}.{name}"),
                name.clone(),
                name.clone(),
            )
        })
        .collect()
}

/// A section completion item labeled `label`, documented from `path`.
fn section_item(schema: &SchemaIndex, path: &str, label: String, insert: String) -> CompletionItem {
    let doc = describe_section(schema, path)
        .map(|(_, body)| body)
        .filter(|body| !body.is_empty())
        .map(doc_markup);
    CompletionItem {
        insert_text: Some(insert),
        label,
        kind: Some(CompletionItemKind::MODULE),
        documentation: doc,
        ..CompletionItem::default()
    }
}

/// Field-name completion for the table at `path`.
fn field_items(
    schema: &SchemaIndex,
    path: &str,
    replace: Option<lsp_types::Range>,
    snippets: bool,
) -> Vec<CompletionItem> {
    let Some(table) = schema.table(path) else {
        return Vec::new();
    };
    table
        .fields
        .iter()
        .map(|(name, field)| {
            let (new_text, format) = field_insert(name, field, snippets);
            edit_item(
                name.clone(),
                new_text,
                format,
                replace,
                None,
                field.description.clone(),
            )
        })
        .collect()
}

/// The insert text for a completed field key, scaffolding the value shape
/// its schema type calls for: `"$0"` for strings, a bare tab stop otherwise.
/// A table-typed field continues as a dotted key path (`output.`), which
/// re-triggers completion. Without client snippet support everything falls
/// back to the plain `name = `.
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
    let scaffold = if field.field_type == FieldType::String {
        "\"$0\""
    } else {
        "$0"
    };
    (
        format!("{name} = {scaffold}"),
        Some(InsertTextFormat::SNIPPET),
    )
}

/// Value completion for `key` in `section`: the values the schema lists.
fn value_items(
    schema: &SchemaIndex,
    section: &str,
    key: &str,
    in_string: bool,
) -> Vec<CompletionItem> {
    let Some((table, field)) = field_of(schema, section, key) else {
        return Vec::new();
    };
    let Some(field) = table.fields.get(&field) else {
        return Vec::new();
    };
    field
        .values
        .iter()
        .filter(|(_, quoted)| *quoted || !in_string)
        .map(|(value, quoted)| CompletionItem {
            label: value.clone(),
            kind: Some(CompletionItemKind::VALUE),
            insert_text: Some(if *quoted && !in_string {
                format!("\"{value}\"")
            } else {
                value.clone()
            }),
            ..CompletionItem::default()
        })
        .collect()
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

    fn complete(
        text: &str,
        line: u32,
        character: u32,
        snippets: bool,
    ) -> Vec<lsp_types::CompletionItem> {
        completion(
            &LineIndex::new(text),
            &SchemaIndex::build(),
            text,
            Position::new(line, character),
            None,
            snippets,
        )
    }

    #[test]
    fn hover_describes_a_nested_section_header() {
        let text = "[output.task]\n";
        let result = hover(
            &LineIndex::new(text),
            &SchemaIndex::build(),
            text,
            Position::new(0, 3),
        );
        assert!(result.is_some(), "expected hover on [output.task]");
    }

    #[test]
    fn hover_describes_a_field_of_a_task_table() {
        let text = "[tasks.build.runtime]\njavascript = \"bun\"\n";
        let result = hover(
            &LineIndex::new(text),
            &SchemaIndex::build(),
            text,
            Position::new(1, 2),
        );
        let Some(lsp_types::Hover {
            contents: lsp_types::HoverContents::Markup(markup),
            ..
        }) = result
        else {
            panic!("expected a hover");
        };
        assert!(markup.value.contains("JavaScript"), "{}", markup.value);
    }

    #[test]
    fn header_completion_offers_every_declared_table() {
        let names_owned = complete("[\n", 0, 1, false);
        let names = labels(&names_owned);
        for expected in [
            "install",
            "output",
            "output.task",
            "output.parallel",
            "tasks",
        ] {
            assert!(names.contains(&expected), "{expected}: {names:?}");
        }
        assert!(!names.contains(&"pm"), "{names:?}");
    }

    #[test]
    fn dotted_header_completion_offers_only_the_parents_subtables() {
        let items = complete("[output.\n", 0, 8, false);
        assert_eq!(labels(&items), ["parallel", "task", "tool"], "{items:?}");
    }

    #[test]
    fn dotted_header_without_subtables_completes_nothing() {
        let items = complete("[install.\n", 0, 9, false);
        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn completion_offers_field_names_in_a_section() {
        let items = complete("[install]\n\n", 1, 0, false);
        assert_eq!(labels(&items), ["frozen", "scripts", "tools"]);
    }

    #[test]
    fn a_task_table_offers_its_own_fields() {
        let items = complete("[tasks.build]\n\n", 1, 0, false);
        let names = labels(&items);
        for expected in ["env", "output", "pm", "runtime", "source"] {
            assert!(names.contains(&expected), "{expected}: {names:?}");
        }
    }

    #[test]
    fn value_completion_offers_the_schema_enum() {
        let items = complete("[tasks.build]\npm = \n", 1, 5, false);
        let names = labels(&items);
        assert!(
            names.contains(&"bun") && names.contains(&"pnpm"),
            "{names:?}"
        );
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("\"bun\""));
    }

    #[test]
    fn value_completion_inside_an_open_string_stays_bare() {
        let items = complete("[tasks.build]\npm = \"b\n", 1, 7, false);
        let bun = items.iter().find(|i| i.label == "bun").expect("bun item");
        assert_eq!(bun.insert_text.as_deref(), Some("bun"));
    }

    #[test]
    fn a_mixed_enum_quotes_only_its_strings() {
        let items = complete("download = \n", 0, 11, false);
        let inserts: Vec<&str> = items
            .iter()
            .filter_map(|i| i.insert_text.as_deref())
            .collect();
        assert_eq!(inserts, ["true", "false", "\"ask\""]);
    }

    #[test]
    fn bool_value_completions_stay_unquoted() {
        let items = complete("[install]\nfrozen = \n", 1, 9, false);
        let inserts: Vec<&str> = items
            .iter()
            .filter_map(|i| i.insert_text.as_deref())
            .collect();
        assert_eq!(inserts, ["true", "false"]);
    }

    #[test]
    fn a_dotted_key_completes_the_fields_of_its_table() {
        let items = complete("[output]\ntask.\n", 1, 5, false);
        assert_eq!(labels(&items), ["stderr", "stdout"]);
        let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("expected a text edit");
        };
        assert_eq!(
            (edit.range.start.character, edit.range.end.character),
            (5, 5)
        );
    }

    #[test]
    fn a_dotted_key_value_completes_from_its_field() {
        let items = complete("[tasks.build]\nruntime.javascript = \n", 1, 21, false);
        assert!(labels(&items).contains(&"deno"), "{:?}", labels(&items));
    }

    #[test]
    fn key_completion_replaces_the_typed_token() {
        let items = complete("[install]\nfro\n", 1, 3, false);
        let item = items.iter().find(|i| i.label == "frozen").expect("frozen");
        let Some(lsp_types::CompletionTextEdit::Edit(edit)) = &item.text_edit else {
            panic!("expected a plain text edit: {item:?}");
        };
        assert_eq!(
            (edit.range.start.character, edit.range.end.character),
            (0, 3)
        );
        assert_eq!(edit.new_text, "frozen = ");
    }

    #[test]
    fn key_token_survives_multibyte_whitespace() {
        let items = complete("[install]\n\u{3000}fr\n", 1, 3, false);
        assert!(labels(&items).contains(&"frozen"), "{:?}", labels(&items));
    }

    #[test]
    fn snippets_scaffold_each_value_shape() {
        let bools = complete("[install]\nfr\n", 1, 2, true);
        let frozen = bools.iter().find(|i| i.label == "frozen").expect("frozen");
        assert_eq!(frozen.insert_text.as_deref(), Some("frozen = $0"));
        let strings = complete("[tasks.build]\np\n", 1, 1, true);
        let pm = strings.iter().find(|i| i.label == "pm").expect("pm");
        assert_eq!(pm.insert_text.as_deref(), Some("pm = \"$0\""));
        assert_eq!(
            pm.insert_text_format,
            Some(lsp_types::InsertTextFormat::SNIPPET)
        );
        let tables = complete("[tasks.build]\nou\n", 1, 2, true);
        let output = tables.iter().find(|i| i.label == "output").expect("output");
        assert_eq!(output.insert_text.as_deref(), Some("output."));
        assert_eq!(output.insert_text_format, None);
    }

    #[test]
    fn without_snippet_support_key_completion_stays_plain() {
        let items = complete("[tasks.build]\np\n", 1, 1, false);
        let pm = items.iter().find(|i| i.label == "pm").expect("pm");
        assert_eq!(pm.insert_text.as_deref(), Some("pm = "));
    }

    #[test]
    fn task_headers_and_keys_complete_project_task_names() {
        use crate::tool::test_support::TempDir;

        let dir = TempDir::new("lsp-task-names");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "dev": "vite", "build:web": "vite build" } }"#,
        )
        .expect("package.json should be written");
        let schema = SchemaIndex::build();
        let header = "[tasks.\n";
        let items = completion(
            &LineIndex::new(header),
            &schema,
            header,
            Position::new(0, 7),
            Some(dir.path()),
            false,
        );
        let web = items
            .iter()
            .find(|i| i.label == "build:web")
            .expect("build:web");
        assert_eq!(web.insert_text.as_deref(), Some("\"build:web\""));
        let keys = "[tasks]\n\n";
        let items = completion(
            &LineIndex::new(keys),
            &schema,
            keys,
            Position::new(1, 0),
            Some(dir.path()),
            false,
        );
        let dev = items.iter().find(|i| i.label == "dev").expect("dev");
        assert_eq!(dev.insert_text.as_deref(), Some("dev."));
    }

    #[test]
    fn comment_line_completes_nothing() {
        let items = complete("[install]\n# frozen = \n", 1, 11, false);
        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn trailing_comment_completes_nothing_but_the_value_before_it_still_does() {
        let text = "[tasks.build]\npm =  # pick one\n";
        let after_comment = complete(text, 1, 8, false);
        assert!(after_comment.is_empty(), "{:?}", labels(&after_comment));
        assert!(labels(&complete(text, 1, 5, false)).contains(&"bun"));
    }

    #[test]
    fn hover_in_a_comment_is_silent() {
        let text = "[install]\n# frozen\n";
        let result = hover(
            &LineIndex::new(text),
            &SchemaIndex::build(),
            text,
            Position::new(1, 4),
        );
        assert!(result.is_none(), "{result:?}");
    }

    #[test]
    fn removed_sections_are_not_offered() {
        let items = complete("[\n", 0, 1, false);
        for removed in [
            "pm",
            "github",
            "parallel",
            "resolution",
            "defaults",
            "task_runner",
        ] {
            assert!(!labels(&items).contains(&removed), "{removed}");
        }
    }
}
