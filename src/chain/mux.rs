//! Line-prefix multiplexer for parallel chain output. Captures each
//! task's stdout/stderr, prefixes lines with `[<task-name>]`, and
//! writes to the parent terminal. Color and prefix-padding are derived
//! from the set of task names supplied up front.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use colored::{Color, Colorize};

static BUFFER_SINK_ID: AtomicU64 = AtomicU64::new(0);

const STREAM_STDOUT: u8 = 0;
const STREAM_STDERR: u8 = 1;

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

/// Production sink. Locks `std::io::stdout` / `std::io::stderr` per
/// line. Zero-sized; share via `Arc::new(StdioSink)`.
pub(crate) struct StdioSink;

impl LineSink for StdioSink {
    fn emit(&self, prefix: &str, is_stderr: bool, line: &str) {
        use std::io::Write;
        if is_stderr {
            let mut h = io::stderr().lock();
            let _ = writeln!(h, "{prefix} {line}");
        } else {
            let mut h = io::stdout().lock();
            let _ = writeln!(h, "{prefix} {line}");
        }
    }
}

/// Spooling sink for grouped parallel output. Reader threads append records to
/// a temp file instead of writing live; the caller replays the file once the
/// task finishes. Each record keeps stdout/stderr identity, so grouped mode
/// preserves stream semantics while avoiding unbounded in-memory buffers.
pub(crate) struct BufferSink {
    file: std::sync::Mutex<File>,
    path: PathBuf,
    closed: AtomicBool,
}

impl BufferSink {
    pub(crate) fn new() -> io::Result<Self> {
        let dir = std::env::temp_dir();
        for _ in 0..100 {
            let id = BUFFER_SINK_ID.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(
                "runner-grouped-output-{}-{id}.tmp",
                std::process::id()
            ));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        file: std::sync::Mutex::new(file),
                        path,
                        closed: AtomicBool::new(false),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique runner output buffer",
        ))
    }

    /// Stop accepting late records. Used before replay when descendant
    /// processes keep a pipe open after the direct task process has exited.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    fn file(&self) -> io::Result<std::sync::MutexGuard<'_, File>> {
        self.file
            .lock()
            .map_err(|_| io::Error::other("buffer sink lock poisoned"))
    }

    /// Replay buffered records to their original streams.
    ///
    /// When `neutralize` is set (grouped replay under GitHub Actions), a child
    /// line that is exactly a `::group::<title>` or `::endgroup::` workflow
    /// command at column 0 is rewritten so it can't nest inside, or
    /// prematurely close, runner's own per-task group: the group title is
    /// surfaced as plain text and the endgroup is dropped. All other lines,
    /// including `::warning::`/`::error::`/`::notice::` annotations, replay
    /// verbatim.
    pub(crate) fn replay_to(
        &self,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
        neutralize: bool,
    ) -> io::Result<()> {
        self.close();
        {
            let mut file = self.file()?;
            file.flush()?;
        }
        let mut file = File::open(&self.path)?;

        // Tokens from child `::stop-commands::<token>` directives whose paired
        // `::<token>::` resume should also be dropped (see the neutralize block).
        let mut stopped_tokens: Vec<Vec<u8>> = Vec::new();

        loop {
            let mut stream = [0_u8; 1];
            match file.read_exact(&mut stream) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            let mut len = [0_u8; 8];
            file.read_exact(&mut len)?;
            let len = usize::try_from(u64::from_le_bytes(len)).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "buffer record too large")
            })?;
            let mut bytes = vec![0_u8; len];
            file.read_exact(&mut bytes)?;

            let out: &mut dyn Write = match stream[0] {
                STREAM_STDOUT => &mut *stdout,
                STREAM_STDERR => &mut *stderr,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid buffer stream marker",
                    ));
                }
            };

            // `bytes` is one line including its trailing `\n` (the buffer sink
            // always appends one). Workflow commands are only honoured by
            // Actions at column 0, so an exact prefix match on the newline-free
            // line is what GitHub would actually interpret.
            if neutralize {
                let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);

                // `::stop-commands::<token>` halts ALL workflow-command
                // processing until a matching `::<token>::`. Left in, a child
                // could swallow our own `::endgroup::` (the group would never
                // close) and every later annotation. Drop the directive and
                // remember the token so its paired resume is dropped too; an
                // unpaired resume falls through as a harmless unknown command.
                if let Some(token) = line.strip_prefix(b"::stop-commands::".as_slice()) {
                    if !token.is_empty() {
                        stopped_tokens.push(token.to_vec());
                    }
                    continue;
                }
                if let Some(pos) = stopped_tokens.iter().position(|t| {
                    line.strip_prefix(b"::".as_slice())
                        .and_then(|inner| inner.strip_suffix(b"::".as_slice()))
                        == Some(t.as_slice())
                }) {
                    stopped_tokens.swap_remove(pos);
                    continue;
                }

                // Group title → plain text (drop a titleless `::group::` rather
                // than emit a stray blank line); `::endgroup::` dropped.
                // `::warning::`/`::error::`/`::notice::` annotations fall
                // through verbatim.
                if let Some(title) = line.strip_prefix(b"::group::".as_slice()) {
                    if !title.is_empty() {
                        out.write_all(title)?;
                        out.write_all(b"\n")?;
                    }
                    continue;
                }
                if line.starts_with(b"::endgroup::") {
                    continue;
                }
            }
            out.write_all(&bytes)?;
        }

        stdout.flush()?;
        stderr.flush()
    }
}

impl Drop for BufferSink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl LineSink for BufferSink {
    fn emit(&self, _prefix: &str, is_stderr: bool, line: &str) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }

        let Some(len) = line
            .len()
            .checked_add(1)
            .and_then(|len| u64::try_from(len).ok())
        else {
            return;
        };
        let marker = if is_stderr {
            STREAM_STDERR
        } else {
            STREAM_STDOUT
        };
        let mut record_header = [0_u8; 9];
        record_header[0] = marker;
        record_header[1..].copy_from_slice(&len.to_le_bytes());

        let Ok(mut file) = self.file() else {
            return;
        };
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if file
            .write_all(&record_header)
            .and_then(|()| file.write_all(line.as_bytes()))
            .and_then(|()| file.write_all(b"\n"))
            .is_err()
        {
            self.close();
        }
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
/// spawned threads. The caller joins each handle once the underlying
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
    fn buffer_sink_replays_both_streams_to_original_destinations() {
        let sink = BufferSink::new().expect("buffer sink should open");
        sink.emit("[ignored]", false, "first");
        sink.emit("[ignored]", true, "second");
        sink.close();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        sink.replay_to(&mut stdout, &mut stderr, false)
            .expect("buffer should replay");

        assert_eq!(stdout, b"first\n");
        assert_eq!(stderr, b"second\n");
    }

    #[test]
    fn replay_neutralizes_child_group_commands_when_enabled() {
        let sink = BufferSink::new().expect("buffer sink should open");
        sink.emit("[i]", false, "::group::Building");
        sink.emit("[i]", false, "compiling...");
        sink.emit("[i]", false, "::endgroup::");
        sink.emit("[i]", false, "::group::"); // titleless → dropped, no blank line
        sink.emit("[i]", true, "::warning::heads up");
        sink.close();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        sink.replay_to(&mut stdout, &mut stderr, true)
            .expect("buffer should replay");

        // `::group::Building` → plain `Building`; `::endgroup::` and a titleless
        // `::group::` dropped; a normal line is kept; the `::warning::`
        // annotation stays verbatim.
        assert_eq!(stdout, b"Building\ncompiling...\n");
        assert_eq!(stderr, b"::warning::heads up\n");
    }

    #[test]
    fn replay_without_neutralize_keeps_child_group_commands_verbatim() {
        let sink = BufferSink::new().expect("buffer sink should open");
        sink.emit("[i]", false, "::group::X");
        sink.emit("[i]", false, "::endgroup::");
        sink.close();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        sink.replay_to(&mut stdout, &mut stderr, false)
            .expect("buffer should replay");

        assert_eq!(stdout, b"::group::X\n::endgroup::\n");
    }

    #[test]
    fn replay_neutralizes_stop_commands_and_paired_resume() {
        let sink = BufferSink::new().expect("buffer sink should open");
        // A child trying to disable command processing: the directive AND its
        // matching resume must be dropped, or the parent's later `::endgroup::`
        // (and annotations) would be swallowed.
        sink.emit("[i]", false, "::stop-commands::abc123");
        sink.emit("[i]", false, "real output");
        sink.emit("[i]", false, "::abc123::"); // paired resume → dropped
        sink.emit("[i]", false, "::other::"); // unpaired → harmless, kept
        sink.emit("[i]", true, "::warning::kept");
        sink.close();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        sink.replay_to(&mut stdout, &mut stderr, true)
            .expect("buffer should replay");

        assert_eq!(stdout, b"real output\n::other::\n");
        assert_eq!(stderr, b"::warning::kept\n");
    }

    #[test]
    fn spawn_readers_streams_lines_through_sink() {
        let sink = Arc::new(VecSink::new());
        let stream = io::Cursor::new(b"hello\nworld\n".to_vec());
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
        let out = io::Cursor::new(b"o\n".to_vec());
        let err = io::Cursor::new(b"e\n".to_vec());
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
