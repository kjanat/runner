//! Hover and completion at a cursor, read from the buffer as `toml_parser` parses it.

use std::collections::BTreeMap;
use std::path::Path;

use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, Hover, HoverContents, InsertTextFormat,
    MarkupContent, MarkupKind, Position,
};

use super::schema_index::{FieldDoc, FieldType, SchemaIndex, TableDoc};
use super::syntax::{Document, Key, Place};
use super::text::LineIndex;
use crate::config::{KeyPath, toml_key};
use crate::provider::Named;

/// Build a hover response for the cursor, if it lands on something documented.
pub(super) fn hover(
    index: &LineIndex,
    schema: &SchemaIndex,
    text: &str,
    pos: Position,
) -> Option<Hover> {
    let cursor = Document::parse(text).at(index.offset(text, pos));
    let section = cursor.section.unwrap_or_default();
    let (title, body) = match cursor.place {
        Place::Header(keys) => describe_section(schema, &names(&keys))?,
        Place::Key(keys) => describe_field(schema, &section, &names(&keys))?,
        Place::Value { key, .. } => describe_field(schema, &section, &key)?,
        Place::Empty | Place::Comment => return None,
    };
    Some(Hover {
        contents: HoverContents::Markup(markdown(&title, &body)),
        range: None,
    })
}

fn names(keys: &[Key]) -> Vec<String> {
    keys.iter().map(|key| key.name.clone()).collect()
}

/// Hover/title for a `[section]` header.
fn describe_section(schema: &SchemaIndex, path: &[String]) -> Option<(String, String)> {
    let table = schema.table(path)?;
    Some((
        format!("[{}]", KeyPath(path.to_vec())),
        table.description.unwrap_or_default(),
    ))
}

/// Hover/title for `key` within `section`.
fn describe_field(
    schema: &SchemaIndex,
    section: &[String],
    key: &[String],
) -> Option<(String, String)> {
    let (table, field) = field_of(schema, section, key)?;
    let key_text = KeyPath(key.to_vec()).to_string();
    let title = if section.is_empty() {
        key_text
    } else {
        format!("[{}].{key_text}", KeyPath(section.to_vec()))
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

/// The table holding the last key of `key` in `section`, and that key.
fn field_of(
    schema: &SchemaIndex,
    section: &[String],
    key: &[String],
) -> Option<(TableDoc, String)> {
    let (field, prefix) = key.split_last()?;
    Some((schema.table(&[section, prefix].concat())?, field.clone()))
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
    let offset = index.offset(text, pos);
    let cursor = Document::parse(text).at(offset);
    let section = cursor.section.clone().unwrap_or_default();
    let before = |keys: Vec<Key>| -> Vec<Key> {
        keys.into_iter()
            .filter(|key| key.span.start <= offset)
            .collect()
    };
    match cursor.place {
        Place::Header(keys) => header_items(schema, &before(keys), project_dir, false),
        Place::Value { key, in_string } => value_items(schema, &section, &key, in_string),
        Place::Key(keys) => {
            let keys = before(keys);
            let replace = keys
                .last()
                .map(|key| index.range(text, key.span.start, offset));
            let parent = names(keys.split_last().map_or(&[][..], |(_, parent)| parent));
            entry_items(
                schema,
                &[section, parent].concat(),
                replace,
                project_dir,
                snippets,
            )
        }
        Place::Empty if cursor.section.is_none() => {
            let mut items = field_items(schema, &[], None, snippets);
            items.extend(header_items(schema, &[], project_dir, true));
            items
        }
        Place::Empty => entry_items(schema, &section, None, project_dir, snippets),
        Place::Comment => Vec::new(),
    }
}

/// The keys a table takes: its declared fields, or for `[tasks]` the project's
/// own task names.
fn entry_items(
    schema: &SchemaIndex,
    path: &[String],
    replace: Option<lsp_types::Range>,
    project_dir: Option<&Path>,
    snippets: bool,
) -> Vec<CompletionItem> {
    if path == ["tasks"] {
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

/// Header completion over the `keys` written before the cursor. Past a dot it
/// offers the parent's sub-tables, or for `[tasks.` the project's task names;
/// otherwise every header path the schema declares.
fn header_items(
    schema: &SchemaIndex,
    keys: &[Key],
    project_dir: Option<&Path>,
    bracketed: bool,
) -> Vec<CompletionItem> {
    let Some((_, parent)) = keys.split_last().filter(|(_, parent)| !parent.is_empty()) else {
        return schema
            .header_paths()
            .into_iter()
            .map(|path| {
                let label = KeyPath(path.clone()).to_string();
                let insert = if bracketed {
                    format!("[{label}]")
                } else {
                    label.clone()
                };
                section_item(schema, &path, label, insert)
            })
            .collect();
    };
    let parent = names(parent);
    if parent == ["tasks"] {
        return task_names(project_dir)
            .into_iter()
            .map(|(name, _, _)| {
                let key = toml_key(&name);
                section_item(schema, &parent, name, key)
            })
            .collect();
    }
    let Some(table) = schema.table(&parent) else {
        return Vec::new();
    };
    table
        .fields
        .iter()
        .filter(|(_, field)| field.field_type == FieldType::Table)
        .map(|(name, _)| {
            section_item(
                schema,
                &[parent.clone(), vec![name.clone()]].concat(),
                name.clone(),
                toml_key(name),
            )
        })
        .collect()
}

/// A section completion item labeled `label`, documented from `path`.
fn section_item(
    schema: &SchemaIndex,
    path: &[String],
    label: String,
    insert: String,
) -> CompletionItem {
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
    path: &[String],
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
    let key = toml_key(name);
    if field.field_type == FieldType::Table {
        return (format!("{key}."), None);
    }
    if !snippets {
        return (format!("{key} = "), None);
    }
    let scaffold = if field.field_type == FieldType::String {
        "\"$0\""
    } else {
        "$0"
    };
    (
        format!("{key} = {scaffold}"),
        Some(InsertTextFormat::SNIPPET),
    )
}

/// Value completion for `key` in `section`: the values the schema lists.
fn value_items(
    schema: &SchemaIndex,
    section: &[String],
    key: &[String],
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
    fn a_quoted_task_key_with_dots_resolves_its_tables() {
        for text in [
            "[tasks.\"package.json:build\".runtime]\njavascript = \"bun\"\n",
            "[tasks.'package.json:build'.runtime]\njavascript = \"bun\"\n",
            "[tasks.\"v1.2\"]\nruntime.javascript = \"bun\"\n",
        ] {
            let result = hover(
                &LineIndex::new(text),
                &SchemaIndex::build(),
                text,
                Position::new(1, 3),
            );
            let Some(lsp_types::Hover {
                contents: lsp_types::HoverContents::Markup(markup),
                ..
            }) = result
            else {
                panic!("expected a hover for {text:?}");
            };
            assert!(markup.value.contains("JavaScript"), "{}", markup.value);
        }
        let items = complete(
            "[tasks.\"package.json:build\"]\nruntime.javascript = \n",
            1,
            21,
            false,
        );
        assert!(labels(&items).contains(&"deno"), "{:?}", labels(&items));
        let items = complete("[tasks.\"package.json:build\".\n", 0, 28, false);
        assert!(labels(&items).contains(&"runtime"), "{:?}", labels(&items));
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
