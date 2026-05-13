//! Line-prefix multiplexer for parallel chain output. Captures each
//! task's stdout/stderr, prefixes lines with `[<task-name>]`, and
//! writes to the parent terminal. Color and prefix-padding are derived
//! from the set of task names supplied up front.

use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use colored::{Color, Colorize};

/// One captured line from a task's piped stdio.
#[derive(Debug)]
pub(crate) struct PrefixedLine {
    /// Padded prefix bracket, e.g. `[build ]` — color may be embedded.
    pub prefix: String,
    /// The line content (no trailing `\n`).
    pub line: String,
    /// Whether this line came from the task's stderr.
    pub is_stderr: bool,
}

/// Compute the right-padded width for prefix labels in the chain.
pub(crate) fn prefix_width(names: &[&str]) -> usize {
    names.iter().map(|n| n.chars().count()).max().unwrap_or(0)
}

/// Deterministic ANSI color for a task name, chosen from an 8-color
/// palette so multiple parallel tasks visually distinguish.
pub(crate) fn color_for(name: &str) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Red,
        Color::BrightCyan,
        Color::BrightMagenta,
    ];
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[hash as usize % PALETTE.len()]
}

/// Render a prefix bracket for `name` padded to `width` characters.
/// Skips color when `colorize == false` (NO_COLOR or non-TTY).
pub(crate) fn render_prefix(name: &str, width: usize, colorize: bool) -> String {
    let padded = format!("{name:<width$}");
    let bracketed = format!("[{padded}]");
    if colorize {
        bracketed.color(color_for(name)).to_string()
    } else {
        bracketed
    }
}

/// Spawn reader threads that read line-by-line from each `Read`, prefix
/// the lines, and send them through the returned receiver. The caller
/// must keep the returned join handles alive until the channel closes.
///
/// `streams` is a slice of `(prefix, is_stderr, reader)` tuples.
pub(crate) fn spawn_readers<R>(
    streams: Vec<(String, bool, R)>,
    sender: Sender<PrefixedLine>,
) -> Vec<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    streams
        .into_iter()
        .map(|(prefix, is_stderr, reader)| {
            let tx = sender.clone();
            std::thread::spawn(move || {
                let buf = BufReader::new(reader);
                for line in buf.lines() {
                    let Ok(line) = line else { return };
                    if tx
                        .send(PrefixedLine {
                            prefix: prefix.clone(),
                            line,
                            is_stderr,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn prefix_width_picks_longest() {
        assert_eq!(prefix_width(&["a", "build", "test"]), 5);
        assert_eq!(prefix_width(&[]), 0);
    }

    #[test]
    fn color_for_is_deterministic() {
        assert_eq!(color_for("build"), color_for("build"));
    }

    #[test]
    fn render_prefix_pads_and_brackets() {
        let p = render_prefix("a", 5, false);
        assert_eq!(p, "[a    ]");
    }

    #[test]
    fn render_prefix_colors_when_enabled() {
        // `colored` honors global SHOULD_COLORIZE which is off in non-TTY
        // test runs; force it on for this test only.
        colored::control::set_override(true);
        let p = render_prefix("a", 1, true);
        colored::control::unset_override();
        assert!(p.contains("[a]"));
        assert!(p.contains("\u{1b}["), "expected ANSI escape, got: {p:?}");
    }

    #[test]
    fn spawn_readers_streams_lines_through_channel() {
        let (tx, rx) = channel();
        let stream = std::io::Cursor::new(b"hello\nworld\n".to_vec());
        let handles = spawn_readers(vec![("[t]".into(), false, stream)], tx);
        for h in handles {
            h.join().unwrap();
        }
        let mut got: Vec<String> = rx.iter().map(|p| p.line).collect();
        got.sort();
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }
}
