//! Line-prefix multiplexer for parallel chain output. Captures each
//! task's stdout/stderr, prefixes lines with `[<task-name>]`, and
//! writes to the parent terminal. Color and prefix-padding are derived
//! from the set of task names supplied up front.

use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::thread::JoinHandle;

use colored::{Color, Colorize};

/// Synchronous, thread-safe destination for prefixed chain output. Each
/// `emit` call writes a single line and releases the underlying lock
/// before returning, so `eprintln!` / `println!` from the main thread
/// can interleave with reader threads without the deadlock the old
/// mpsc-plus-dedicated-writer design suffered.
pub(crate) trait LineSink: Send + Sync {
    /// Write `line` to the appropriate stream, prefixed with `prefix`.
    /// Implementations must acquire whatever lock guards the underlying
    /// stream *per call* and release it before returning.
    fn emit(&self, prefix: &str, is_stderr: bool, line: &str);
}

/// Production sink — locks `std::io::stdout` / `std::io::stderr` per
/// line. Zero-sized; share via `Arc::new(StdioSink)`.
pub(crate) struct StdioSink;

impl LineSink for StdioSink {
    fn emit(&self, prefix: &str, is_stderr: bool, line: &str) {
        use std::io::Write;
        if is_stderr {
            let mut h = std::io::stderr().lock();
            let _ = writeln!(h, "{prefix} {line}");
        } else {
            let mut h = std::io::stdout().lock();
            let _ = writeln!(h, "{prefix} {line}");
        }
    }
}

/// Buffering sink for grouped parallel output. Reader threads append their
/// lines here instead of writing live; the caller drains the buffer with
/// [`BufferSink::take`] once the task finishes and emits it inside one
/// collapsible group. Both reader threads share the mutex, so stdout/stderr
/// interleave in arrival order at line granularity. `prefix` and `is_stderr`
/// are ignored — the group title identifies the task and both streams fold
/// into the single group.
#[derive(Default)]
pub(crate) struct BufferSink {
    buf: std::sync::Mutex<Vec<u8>>,
}

impl BufferSink {
    /// Drain the accumulated bytes, leaving the buffer empty.
    pub(crate) fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.buf.lock().unwrap())
    }
}

impl LineSink for BufferSink {
    fn emit(&self, _prefix: &str, _is_stderr: bool, line: &str) {
        let mut buf = self.buf.lock().unwrap();
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }
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
/// Skips color when `colorize == false` (`NO_COLOR` or non-TTY).
pub(crate) fn render_prefix(name: &str, width: usize, colorize: bool) -> String {
    let padded = format!("{name:<width$}");
    let bracketed = format!("[{padded}]");
    if colorize {
        bracketed.color(color_for(name)).to_string()
    } else {
        bracketed
    }
}

/// Spawn one reader thread per `(prefix, is_stderr, reader)` entry in
/// `streams`. Each thread reads its `Read` line-by-line and pushes the
/// result through `sink`. Returns the `Vec<JoinHandle<()>>` for the
/// spawned threads — the caller joins each handle once the underlying
/// pipes close (which happens naturally when each child process exits
/// and the OS tears its stdio fds down).
pub(crate) fn spawn_readers<R>(
    streams: Vec<(String, bool, R)>,
    sink: &Arc<dyn LineSink>,
) -> Vec<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    streams
        .into_iter()
        .map(|(prefix, is_stderr, reader)| {
            let sink = Arc::clone(sink);
            std::thread::spawn(move || {
                let buf = BufReader::new(reader);
                for line in buf.lines() {
                    let Ok(line) = line else { return };
                    sink.emit(&prefix, is_stderr, &line);
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test sink that records every emit into a shared `Vec`. Lets the
    /// reader tests assert order-insensitively without standing up the
    /// production stdio locking path.
    struct VecSink {
        lines: Mutex<Vec<(String, bool, String)>>,
    }

    impl VecSink {
        fn new() -> Self {
            Self {
                lines: Mutex::new(Vec::new()),
            }
        }

        fn take(&self) -> Vec<(String, bool, String)> {
            std::mem::take(&mut *self.lines.lock().unwrap())
        }
    }

    impl LineSink for VecSink {
        fn emit(&self, prefix: &str, is_stderr: bool, line: &str) {
            self.lines
                .lock()
                .unwrap()
                .push((prefix.to_string(), is_stderr, line.to_string()));
        }
    }

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
    fn buffer_sink_accumulates_both_streams_in_order_then_drains() {
        let sink = BufferSink::default();
        sink.emit("[ignored]", false, "first");
        sink.emit("[ignored]", true, "second");
        assert_eq!(sink.take(), b"first\nsecond\n");
        // take() drains, so a second call yields nothing.
        assert!(sink.take().is_empty());
    }

    #[test]
    fn spawn_readers_streams_lines_through_sink() {
        let sink = Arc::new(VecSink::new());
        let stream = std::io::Cursor::new(b"hello\nworld\n".to_vec());
        let handles = spawn_readers(
            vec![("[t]".into(), false, stream)],
            &(Arc::clone(&sink) as Arc<dyn LineSink>),
        );
        for h in handles {
            h.join().unwrap();
        }
        let mut got: Vec<String> = sink.take().into_iter().map(|(_, _, line)| line).collect();
        got.sort();
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn spawn_readers_routes_stderr_flag_through_sink() {
        let sink = Arc::new(VecSink::new());
        let out = std::io::Cursor::new(b"o\n".to_vec());
        let err = std::io::Cursor::new(b"e\n".to_vec());
        let handles = spawn_readers(
            vec![("[t]".into(), false, out), ("[t]".into(), true, err)],
            &(Arc::clone(&sink) as Arc<dyn LineSink>),
        );
        for h in handles {
            h.join().unwrap();
        }
        let mut got = sink.take();
        got.sort_by_key(|(_, is_err, _)| *is_err);
        assert_eq!(got[0].2, "o");
        assert!(!got[0].1);
        assert_eq!(got[1].2, "e");
        assert!(got[1].1);
    }
}
