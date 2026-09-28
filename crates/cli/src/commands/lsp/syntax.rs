//! The buffer's lines as `toml_parser` reads them: headers, key/value lines
//! and comments, with decoded keys and byte spans, including half-typed ones.

use std::ops::Range;

use toml_parser::Source;
use toml_parser::decoder::Encoding;
use toml_parser::parser::{Event, EventKind};

/// One key as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Key {
    /// The decoded key.
    pub name: String,
    /// Where it is written.
    pub span: Range<usize>,
}

/// What a line holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Whitespace or a comment only.
    Blank,
    /// A `[table]` or `[[table]]` header.
    Header,
    /// A key, with or without its `=` and value.
    Entry,
}

/// A value and how it is quoted.
#[derive(Debug, Clone)]
struct Value {
    span: Range<usize>,
    encoding: Option<Encoding>,
    closed: bool,
}

/// One logical line.
#[derive(Debug, Clone)]
struct Line {
    kind: Kind,
    /// The header in effect, for an entry.
    table: Vec<String>,
    keys: Vec<Key>,
    /// Byte offset of the `=`.
    eq: Option<usize>,
    value: Option<Value>,
    comment: Option<usize>,
    /// Where the line starts, at or after the previous newline.
    start: usize,
    /// Where its newline, or the end of the buffer, is.
    end: usize,
    /// The header's `[` through its `]` or last key.
    header: Range<usize>,
}

impl Line {
    const fn new(start: usize) -> Self {
        Self {
            kind: Kind::Blank,
            table: Vec::new(),
            keys: Vec::new(),
            eq: None,
            value: None,
            comment: None,
            start,
            end: start,
            header: start..start,
        }
    }

    fn names(&self) -> Vec<String> {
        self.keys.iter().map(|key| key.name.clone()).collect()
    }

    fn path(&self) -> Vec<String> {
        match self.kind {
            Kind::Header => self.names(),
            Kind::Entry => [self.table.clone(), self.names()].concat(),
            Kind::Blank => Vec::new(),
        }
    }
}

/// Where the cursor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Place {
    /// In a header, over its keys.
    Header(Vec<Key>),
    /// On the key side of an entry, over its keys.
    Key(Vec<Key>),
    /// On the value side of the entry for `key`.
    Value {
        /// The entry's keys.
        key: Vec<String>,
        /// Whether the cursor is inside an open string.
        in_string: bool,
    },
    /// On a line with nothing on it.
    Empty,
    /// In a comment.
    Comment,
}

/// The cursor's place and the header above it.
pub(super) struct Cursor {
    /// The keys of the nearest header above the cursor's line.
    pub section: Option<Vec<String>>,
    pub place: Place,
}

/// A parsed buffer.
pub(super) struct Document {
    lines: Vec<Line>,
}

impl Document {
    pub(super) fn parse(text: &str) -> Self {
        let source = Source::new(text);
        let tokens = source.lex().into_vec();
        let mut events: Vec<Event> = Vec::new();
        toml_parser::parser::parse_document(&tokens, &mut events, &mut ());
        let mut lines = Vec::new();
        let mut line = Line::new(0);
        let mut table: Vec<String> = Vec::new();
        let mut depth = 0usize;
        for event in &events {
            let span = event.span();
            let range = span.start()..span.end();
            match event.kind() {
                EventKind::StdTableOpen | EventKind::ArrayTableOpen => {
                    line.kind = Kind::Header;
                    line.header = range;
                }
                EventKind::StdTableClose | EventKind::ArrayTableClose => {
                    line.header.end = range.end;
                }
                EventKind::InlineTableOpen | EventKind::ArrayOpen => {
                    if depth == 0 {
                        line.value = Some(Value {
                            span: range,
                            encoding: None,
                            closed: true,
                        });
                    }
                    depth += 1;
                }
                EventKind::InlineTableClose | EventKind::ArrayClose => {
                    depth = depth.saturating_sub(1);
                    if depth == 0
                        && let Some(value) = &mut line.value
                    {
                        value.span.end = range.end;
                    }
                }
                EventKind::SimpleKey if depth == 0 && line.eq.is_none() => {
                    if line.kind == Kind::Blank {
                        line.kind = Kind::Entry;
                    }
                    if line.kind == Kind::Header {
                        line.header.end = range.end;
                    }
                    line.keys.push(Key {
                        name: decode_key(&source, *event),
                        span: range,
                    });
                }
                EventKind::KeyValSep if depth == 0 => line.eq = Some(range.start),
                EventKind::Scalar if depth == 0 => {
                    let raw = source.get(*event).map_or("", |raw| raw.as_str());
                    line.value = Some(Value {
                        closed: closed(raw, event.encoding()),
                        span: range,
                        encoding: event.encoding(),
                    });
                }
                EventKind::Comment if depth == 0 => line.comment = Some(range.start),
                EventKind::Newline if depth == 0 => {
                    line.end = range.start;
                    finish(&mut line, &mut table);
                    lines.push(std::mem::replace(&mut line, Line::new(range.end)));
                }
                _ => {}
            }
        }
        line.end = text.len();
        finish(&mut line, &mut table);
        lines.push(line);
        Self { lines }
    }

    /// The cursor at byte `offset`.
    pub(super) fn at(&self, offset: usize) -> Cursor {
        let index = self
            .lines
            .iter()
            .position(|line| line.start <= offset && offset <= line.end);
        let section = self.lines[..index.unwrap_or(self.lines.len())]
            .iter()
            .rev()
            .find(|line| line.kind == Kind::Header)
            .map(Line::names);
        let Some(line) = index.map(|index| &self.lines[index]) else {
            return Cursor {
                section,
                place: Place::Empty,
            };
        };
        if line.comment.is_some_and(|comment| comment <= offset) {
            return Cursor {
                section,
                place: Place::Comment,
            };
        }
        let place = match line.kind {
            Kind::Blank => Place::Empty,
            Kind::Header => Place::Header(line.keys.clone()),
            Kind::Entry => match line.eq {
                Some(eq) if offset > eq => Place::Value {
                    key: line.names(),
                    in_string: line.value.as_ref().is_some_and(|value| {
                        value.encoding.is_some()
                            && value.span.start < offset
                            && (!value.closed || offset < value.span.end)
                    }),
                },
                _ => Place::Key(line.keys.clone()),
            },
        };
        Cursor { section, place }
    }

    /// Where `path` is written: its key, else its header, else those of the
    /// nearest table above it.
    pub(super) fn span_of(&self, path: &[String]) -> Option<Range<usize>> {
        (1..=path.len()).rev().find_map(|len| {
            let path = &path[..len];
            self.lines
                .iter()
                .find(|line| line.kind == Kind::Entry && line.path() == path)
                .and_then(|line| line.keys.last().map(|key| key.span.clone()))
                .or_else(|| {
                    self.lines
                        .iter()
                        .find(|line| line.kind == Kind::Header && line.path() == path)
                        .map(|line| line.header.clone())
                })
        })
    }
}

/// Close `line`: an entry takes the header in effect, and a header becomes it.
fn finish(line: &mut Line, table: &mut Vec<String>) {
    match line.kind {
        Kind::Header => *table = line.names(),
        Kind::Entry => line.table.clone_from(table),
        Kind::Blank => {}
    }
}

fn decode_key(source: &Source<'_>, event: Event) -> String {
    let mut name = String::new();
    if let Some(raw) = source.get(event) {
        raw.decode_key(&mut name, &mut ());
    }
    name
}

/// Whether a scalar written as `raw` has its closing quotes.
fn closed(raw: &str, encoding: Option<Encoding>) -> bool {
    let quote = match encoding {
        None => return true,
        Some(Encoding::BasicString) => "\"",
        Some(Encoding::LiteralString) => "'",
        Some(Encoding::MlBasicString) => "\"\"\"",
        Some(Encoding::MlLiteralString) => "'''",
    };
    raw.len() >= 2 * quote.len() && raw.ends_with(quote)
}

#[cfg(test)]
mod tests {
    use super::{Document, Place};

    fn names(place: &Place) -> Vec<&str> {
        match place {
            Place::Header(keys) | Place::Key(keys) => {
                keys.iter().map(|key| key.name.as_str()).collect()
            }
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_quoted_key_keeps_its_dots() {
        let text = "[tasks.\"package.json:build\".runtime]\njavascript = \"bun\"\n";
        let document = Document::parse(text);
        let cursor = document.at(text.len() - 3);
        assert_eq!(
            cursor.section.as_deref(),
            Some(
                &[
                    "tasks".to_owned(),
                    "package.json:build".to_owned(),
                    "runtime".to_owned()
                ][..]
            )
        );
        assert_eq!(
            cursor.place,
            Place::Value {
                key: vec!["javascript".to_owned()],
                in_string: true
            }
        );
        let header = document.at(10);
        assert_eq!(
            names(&header.place),
            ["tasks", "package.json:build", "runtime"]
        );
    }

    #[test]
    fn a_half_typed_header_reads_as_its_keys() {
        let text = "[tasks.\n";
        let cursor = Document::parse(text).at(7);
        assert_eq!(names(&cursor.place), ["tasks", ""]);
    }

    #[test]
    fn a_path_is_found_at_its_key_else_its_table() {
        let text = "[tasks.\"a.b\"]\npm = \"x\"\n[output]\n";
        let document = Document::parse(text);
        let key = document
            .span_of(&["tasks".into(), "a.b".into(), "pm".into()])
            .expect("the pm key");
        assert_eq!(&text[key], "pm");
        let table = document
            .span_of(&["output".into(), "nope".into()])
            .expect("its table");
        assert_eq!(&text[table], "[output]");
    }
}
