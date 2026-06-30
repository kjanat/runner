//! Byte-offset ↔ LSP [`Position`] mapping and small line helpers.
//!
//! LSP positions are `(line, character)` with `character` counted in UTF-16
//! code units; the rest of the server works in byte offsets into the document
//! text. [`LineIndex`] bridges the two so diagnostics (byte spans from the TOML
//! parser) and hover/completion (incoming cursor positions) speak a common
//! coordinate.

use lsp_types::{Position, Range};

/// Precomputed line-start byte offsets for one document revision.
pub(super) struct LineIndex {
    /// Byte offset of the start of each line; always begins with `0`.
    line_starts: Vec<usize>,
    /// Total length of the indexed text in bytes.
    len: usize,
}

impl LineIndex {
    /// Build an index over `text`.
    pub(super) fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|&(_, b)| b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self {
            line_starts,
            len: text.len(),
        }
    }

    /// Convert a byte `offset` into `text` to an LSP [`Position`]. Offsets past
    /// the end clamp to the document end.
    pub(super) fn position(&self, text: &str, offset: usize) -> Position {
        let offset = offset.min(self.len);
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        let line_start = self.line_starts[line];
        let col16: u32 = text[line_start..offset]
            .chars()
            .map(|c| u32::try_from(c.len_utf16()).unwrap_or(1))
            .sum();
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: col16,
        }
    }

    /// Convert an LSP [`Position`] to a byte offset into `text`. A line or
    /// column past the end clamps to the nearest valid boundary.
    pub(super) fn offset(&self, text: &str, pos: Position) -> usize {
        let line = pos.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.len;
        };
        let line_end = self
            .line_starts
            .get(line + 1)
            .map_or(self.len, |&next| next);
        let mut col16 = 0u32;
        for (rel, c) in text[line_start..line_end].char_indices() {
            if col16 >= pos.character {
                return line_start + rel;
            }
            col16 += u32::try_from(c.len_utf16()).unwrap_or(1);
        }
        line_end
    }

    /// The LSP [`Range`] spanning the half-open byte range `[start, end)`.
    pub(super) fn range(&self, text: &str, start: usize, end: usize) -> Range {
        Range {
            start: self.position(text, start),
            end: self.position(text, end),
        }
    }

    /// The [`Range`] covering line `line` (its content, excluding the newline).
    /// An out-of-bounds line yields a zero-width range at the document end.
    pub(super) fn line_range(&self, text: &str, line: usize) -> Range {
        let Some(&start) = self.line_starts.get(line) else {
            let end = self.position(text, self.len);
            return Range { start: end, end };
        };
        let end = self
            .line_starts
            .get(line + 1)
            .map_or(self.len, |&next| next.saturating_sub(1));
        self.range(text, start, end)
    }
}

/// Locate the byte range of a `[section]` (or `[a.b]`) header line in `text`,
/// matching the dotted `path` exactly (whitespace-insensitive). Returns the
/// range of the header line's content. Used to anchor a section-level
/// diagnostic when the parser gives no span of its own.
pub(super) fn find_header_range(index: &LineIndex, text: &str, path: &str) -> Option<Range> {
    for (line, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            && inner.trim() == path
        {
            return Some(index.line_range(text, line));
        }
    }
    None
}

/// Locate the byte range of a bare `key` assignment (`key = ...`) under the
/// section whose header is `section` (or anywhere, when `section` is `None`).
/// Returns the range of the `key` token itself. Best-effort: the first match
/// wins.
pub(super) fn find_key_range(
    index: &LineIndex,
    text: &str,
    section: Option<&str>,
    key: &str,
) -> Option<Range> {
    let mut current: Option<String> = None;
    for (line, raw) in text.lines().enumerate() {
        let trimmed = raw.trim_start();
        if let Some(inner) = trimmed
            .trim_end()
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
        {
            current = Some(inner.trim().to_string());
            continue;
        }
        if section.is_some_and(|want| current.as_deref() != Some(want)) {
            continue;
        }
        let Some((lhs, _)) = trimmed.split_once('=') else {
            continue;
        };
        if lhs.trim() == key {
            // Column of the key token = leading whitespace of the raw line.
            let indent = raw.len() - trimmed.len();
            let key_start_col = indent + (lhs.len() - lhs.trim_start().len());
            let line_start = position_line_start(index, line);
            let start = line_start + key_start_col;
            return Some(index.range(text, start, start + key.len()));
        }
    }
    None
}

/// Byte offset of the start of `line` (0 when out of bounds is impossible here
/// because callers iterate existing lines).
fn position_line_start(index: &LineIndex, line: usize) -> usize {
    index.line_starts.get(line).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{LineIndex, find_header_range, find_key_range};

    #[test]
    fn position_and_offset_round_trip() {
        let text = "[pm]\nnode = \"bun\"\n";
        let index = LineIndex::new(text);
        // Byte 5 is the start of line 1 (`node`).
        let pos = index.position(text, 5);
        assert_eq!((pos.line, pos.character), (1, 0));
        assert_eq!(index.offset(text, pos), 5);
    }

    #[test]
    fn finds_header_and_key_ranges() {
        let text = "[pm]\nnode = \"bun\"\n";
        let index = LineIndex::new(text);
        assert!(find_header_range(&index, text, "pm").is_some());
        assert!(find_key_range(&index, text, Some("pm"), "node").is_some());
        // Same key, wrong section → no match.
        assert!(find_key_range(&index, text, Some("tasks"), "node").is_none());
    }
}
