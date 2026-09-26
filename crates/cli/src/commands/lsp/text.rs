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

    /// The [`Range`] covering line `line` (its content, excluding the newline,
    /// and a preceding `\r` on CRLF buffers). An out-of-bounds line yields a
    /// zero-width range at the document end.
    pub(super) fn line_range(&self, text: &str, line: usize) -> Range {
        let Some(&start) = self.line_starts.get(line) else {
            let end = self.position(text, self.len);
            return Range { start: end, end };
        };
        let end = self
            .line_starts
            .get(line + 1)
            .map_or(self.len, |&next| next.saturating_sub(1));
        let end = if end > start && text.as_bytes()[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        self.range(text, start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    #[test]
    fn position_and_offset_round_trip() {
        let text = "[env]\nCI = \"1\"\n";
        let index = LineIndex::new(text);
        // Byte 6 is the start of line 1 (`CI`).
        let pos = index.position(text, 6);
        assert_eq!((pos.line, pos.character), (1, 0));
        assert_eq!(index.offset(text, pos), 6);
    }

    #[test]
    fn line_range_excludes_trailing_carriage_return() {
        let text = "[env]\r\nCI = \"1\"\r\n";
        let index = LineIndex::new(text);
        let range = index.line_range(text, 0);
        assert_eq!(
            (range.start.character, range.end.character),
            (0, 5),
            "{range:?}"
        );
    }
}
