//! Live stderr with an invocation-wide, Actions-only failure recap.
//!
//! Stdout never passes through this collector. Children inherit a marker so a
//! nested runner's diagnostics are collected once, by its outer task.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::{FailureReplay, ReplayOutput, SuccessReplay};
use crate::resolver::ResolutionOverrides;

#[derive(Debug, Default)]
pub(crate) struct Session(Mutex<Vec<Arc<Capture>>>);

#[derive(Debug)]
pub(crate) struct Capture {
    name: String,
    policy: ReplayOutput,
    file: Mutex<File>,
    code: Mutex<Option<i32>>,
    lost: AtomicBool,
    closed: AtomicBool,
}

pub(crate) struct Tee {
    pub inner: Arc<dyn crate::chain::mux::LineSink>,
    pub capture: Option<Arc<Capture>>,
}

impl crate::chain::mux::LineSink for Tee {
    fn emit(&self, prefix: &str, is_stderr: bool, line: &str) -> io::Result<()> {
        let written = self.inner.emit(prefix, is_stderr, line);
        if is_stderr && let Some(capture) = &self.capture {
            let _ = capture.append(format!("{line}\n").as_bytes());
        }
        written
    }

    fn emit_raw(&self, is_stderr: bool, bytes: &[u8]) -> io::Result<()> {
        let written = self.inner.emit_raw(is_stderr, bytes);
        if is_stderr && let Some(capture) = &self.capture {
            let _ = capture.append(bytes);
        }
        written
    }
}

impl Session {
    pub(crate) fn start(
        &self,
        overrides: &ResolutionOverrides,
        key: &str,
        name: &str,
    ) -> io::Result<Option<Arc<Capture>>> {
        if !crate::commands::collects_replay(overrides)
            || !overrides.emits_groups_for(key)
            || overrides.dry_run
            || (overrides.output.replay.failure == FailureReplay::Off
                && overrides.output.replay.success == SuccessReplay::Off)
        {
            return Ok(None);
        }
        let capture = Arc::new(Capture {
            name: name.to_owned(),
            policy: overrides.output.replay,
            file: Mutex::new(tempfile::tempfile()?),
            code: Mutex::new(None),
            lost: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        self.0.lock().unwrap().push(Arc::clone(&capture));
        Ok(Some(capture))
    }

    /// Called after all task execution and chain summaries, including on error.
    pub(crate) fn finish(&self) -> io::Result<()> {
        let captures = std::mem::take(&mut *self.0.lock().unwrap());
        let failures = captures
            .iter()
            .filter(|capture| capture.code.lock().unwrap().is_some_and(|code| code != 0))
            .count();
        let mut stderr = io::stderr().lock();
        for capture in captures {
            capture.render(failures, &mut stderr)?;
        }
        stderr.flush()
    }
}

impl Capture {
    pub(crate) fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let mut file = self.file.lock().unwrap();
        if self.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let written = file.write_all(bytes);
        drop(file);
        if written.is_err() {
            self.lost.store(true, Ordering::SeqCst);
        }
        written
    }

    pub(crate) fn complete(&self, code: i32) {
        *self.code.lock().unwrap() = Some(code);
    }

    fn render(&self, failures: usize, out: &mut impl Write) -> io::Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        let Some(code) = *self.code.lock().unwrap() else {
            return Ok(()); // A task that never started or was cancelled is not a failure.
        };
        let grouped = if code == 0 {
            match self.policy.success {
                SuccessReplay::Off => return Ok(()),
                SuccessReplay::Plain => false,
                SuccessReplay::Grouped => true,
            }
        } else {
            match self.policy.failure {
                FailureReplay::Off => return Ok(()),
                FailureReplay::Plain => false,
                FailureReplay::Grouped => true,
                FailureReplay::Auto => failures > 1,
            }
        };
        let mut file = self.file.lock().unwrap();
        let length = file.seek(SeekFrom::End(0))?;
        if code == 0 && length == 0 {
            return Ok(());
        }
        file.seek(SeekFrom::Start(0))?;
        // Treat task names as text too: neither newlines nor workflow commands
        // from a name may alter the recap's structure.
        let name = self
            .name
            .replace(['\r', '\n'], " ")
            .replace("::", ": :")
            .replace("##[", "## [");
        let title = format!("{name} — exit {code}");
        // Live stderr can end mid-line. Workflow commands must begin on a
        // fresh line, and a plain failure title needs its own line too.
        writeln!(out)?;
        if grouped {
            writeln!(
                out,
                "{}",
                actions_rs::WorkflowCommand::new("group").message(&title)
            )?;
        } else {
            inert(title.as_bytes(), out)?;
        }
        let replay = inert(&mut *file, out);
        drop(file);
        if self.lost.load(Ordering::SeqCst) {
            writeln!(out, "runner: some output could not be retained for replay")?;
        }
        // Close even when reading the spool failed.
        if grouped {
            writeln!(out, "::endgroup::")?;
        }
        replay
    }
}

/// Neutralize both workflow-command syntaxes, including commands containing
/// properties and legacy commands embedded mid-line. Bounded even for a child
/// that writes gigabytes without a newline. Live output remains byte-for-byte.
fn inert(input: impl Read, out: &mut impl Write) -> io::Result<()> {
    let mut previous = [0; 2];
    let mut last = None;
    let mut reader = BufReader::new(input);
    let mut buffered = io::BufWriter::new(out);
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if chunk.is_empty() {
            break;
        }
        let mut start = 0;
        for (index, &byte) in chunk.iter().enumerate() {
            if (byte == b':' && previous[1] == b':') || (byte == b'[' && previous == *b"##") {
                buffered.write_all(&chunk[start..index])?;
                buffered.write_all(b" ")?;
                start = index;
            }
            previous = [previous[1], byte];
        }
        buffered.write_all(&chunk[start..])?;
        last = chunk.last().copied();
        let consumed = chunk.len();
        reader.consume(consumed);
    }
    if last.is_some_and(|byte| byte != b'\n') {
        buffered.write_all(b"\n")?;
    }
    buffered.flush()
}

/// Arrange to tee the child's stderr, unless the user discarded that stream.
pub(crate) fn prepare(
    command: &mut Command,
    overrides: &ResolutionOverrides,
    key: &str,
    name: &str,
) -> io::Result<Option<Arc<Capture>>> {
    let capture = overrides.replay.start(overrides, key, name)?;
    if capture.is_some() && overrides.task_streams_for(key).1 == crate::tool::TaskStream::Inherit {
        command.stderr(Stdio::piped());
    }
    Ok(capture)
}

pub(crate) fn wait(child: &mut Child, capture: Option<Arc<Capture>>) -> io::Result<ExitStatus> {
    let mut readers = Vec::new();
    if let Some(capture) = &capture
        && let Some(mut stderr) = child.stderr.take()
    {
        let capture = Arc::clone(capture);
        readers.push(std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match stderr.read(&mut bytes) {
                    Ok(0) => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        capture.lost.store(true, Ordering::SeqCst);
                        break;
                    }
                    Ok(count) => {
                        // Never hold the stderr lock while acquiring the spool
                        // lock: the final recap holds them in the other order.
                        let _ = io::stderr().lock().write_all(&bytes[..count]);
                        let _ = capture.append(&bytes[..count]);
                    }
                }
            }
        }));
    }
    let result = child.wait();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    crate::chain::exec::wait_for_readers(&mut readers, std::time::Duration::from_millis(250));
    if let (Ok(status), Some(capture)) = (&result, capture) {
        capture.complete(crate::commands::exit_code(*status));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::inert;
    use std::io::{self, Read};

    /// Force a pipe-like reader to split commands at every possible offset.
    struct Chunks<'a> {
        bytes: &'a [u8],
        size: usize,
    }

    impl Read for Chunks<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let size = out.len().min(self.size);
            self.bytes.read(&mut out[..size])
        }
    }

    #[test]
    fn replay_neutralizes_commands_across_chunk_boundaries() {
        let input = b"::error file=x::bad\nprefix ##[error]bad\n::: ###[:\xff";
        let expected = b": :error file=x: :bad\nprefix ## [error]bad\n: : : ### [:\xff\n";
        for size in 1..=input.len() {
            let mut output = Vec::new();
            inert(Chunks { bytes: input, size }, &mut output).unwrap();
            assert_eq!(output, expected, "chunk size {size}");
        }
    }

    #[test]
    fn replay_preserves_empty_output_and_existing_final_newlines() {
        for (input, expected) in [
            (b"".as_slice(), b"".as_slice()),
            (b"\n", b"\n"),
            (b"last line\n", b"last line\n"),
            (b"last line", b"last line\n"),
            (b"\xff", b"\xff\n"),
        ] {
            let mut output = Vec::new();
            inert(
                Chunks {
                    bytes: input,
                    size: 1,
                },
                &mut output,
            )
            .unwrap();
            assert_eq!(output, expected);
        }
    }
}
