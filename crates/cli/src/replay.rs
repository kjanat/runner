//! Live stderr with an invocation-wide, Actions-only failure recap.
//!
//! Stdout never passes through this collector. Children inherit a marker so a
//! nested runner's diagnostics are collected once, by its outer task.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
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
            || !overrides.executes()
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
        let title = format!("{} — exit {code}", self.name);
        // Live stderr can end mid-line. Workflow commands must begin on a
        // fresh line, and a plain failure title needs its own line too.
        writeln!(out)?;
        if grouped {
            actions_rs::log::group_to(title, out, |group| {
                self.write_replay(&mut *file, None, group)
            })
        } else {
            self.write_replay(&mut *file, Some(&title), out)
        }
    }

    /// Preserve replay bytes while preventing annotations and other workflow
    /// commands from executing again. The plain title needs the same protection.
    fn write_replay(
        &self,
        input: &mut impl Read,
        title: Option<&str>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let mut stopped = actions_rs::log::stop_commands_to(out)?;
        let replay = (|| {
            if let Some(title) = title {
                writeln!(stopped, "{}", title.replace(['\r', '\n'], " "))?;
            }
            io::copy(input, &mut stopped).map(|_| ())
        })();
        // Resume commands even if reading the spool failed, before the enclosing
        // group closes. Observe closing errors without masking a replay error.
        let resumed = stopped.finish().and_then(|out| {
            if self.lost.load(Ordering::SeqCst) {
                writeln!(out, "runner: some output could not be retained for replay")?;
            }
            Ok(())
        });
        replay.and(resumed)
    }
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
    use super::Capture;
    use crate::config::{FailureReplay, ReplayOutput};
    use std::io::{self, Read};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    fn capture(name: &str, failure: FailureReplay) -> Capture {
        Capture {
            name: name.to_owned(),
            policy: ReplayOutput {
                failure,
                ..ReplayOutput::default()
            },
            file: Mutex::new(tempfile::tempfile().unwrap()),
            code: Mutex::new(Some(7)),
            lost: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    fn render_failure(name: &str, failure: FailureReplay) -> Vec<u8> {
        let capture = capture(name, failure);
        capture.append(b"failure details\n").unwrap();
        let mut output = Vec::new();
        capture.render(1, &mut output).unwrap();
        output
    }

    fn suspended_body(output: &[u8]) -> (&[u8], &[u8]) {
        let newline = output.iter().position(|byte| *byte == b'\n').unwrap();
        let token = std::str::from_utf8(&output[..newline])
            .unwrap()
            .strip_prefix("::stop-commands::")
            .unwrap();
        assert_ne!(token, "");
        let closing = format!("\n::{token}::\n");
        let end = output
            .windows(closing.len())
            .position(|bytes| bytes == closing.as_bytes())
            .unwrap();
        (&output[newline + 1..=end], &output[end + closing.len()..])
    }

    #[test]
    fn grouped_title_preserves_text_with_command_data_escaping() {
        let output = render_failure(
            "build::test %0A\r\n::error::injected\n##[error]legacy",
            FailureReplay::Grouped,
        );
        let header =
            "\n::group::build::test %250A%0D%0A::error::injected%0A##[error]legacy — exit 7\n";
        let (body, tail) = suspended_body(output.strip_prefix(header.as_bytes()).unwrap());
        assert_eq!(body, b"failure details\n");
        assert_eq!(tail, b"::endgroup::\n");
    }

    #[test]
    fn plain_title_is_one_inert_line_without_command_encoding() {
        let output = render_failure(
            "build::test %0A\r\n::error::injected\n##[error]legacy",
            FailureReplay::Plain,
        );
        let (body, tail) = suspended_body(output.strip_prefix(b"\n").unwrap());
        assert_eq!(
            body,
            concat!(
                "build::test %0A  ::error::injected ##[error]legacy — exit 7\n",
                "failure details\n",
            )
            .as_bytes(),
        );
        assert_eq!(tail, b"");
    }

    #[test]
    fn replay_preserves_bytes_and_separates_the_resume_marker() {
        for mode in [FailureReplay::Plain, FailureReplay::Grouped] {
            for input in [
                b"".as_slice(),
                b"\n",
                b"last line\n",
                b"last line",
                b"\xff",
                b"::error file=x::bad\nprefix ##[error]bad\n::: ###[:\xff",
            ] {
                let capture = capture("build", mode);
                capture.append(input).unwrap();
                let mut output = Vec::new();
                capture.render(1, &mut output).unwrap();
                let (header, mut expected, tail) = if mode == FailureReplay::Grouped {
                    (
                        "\n::group::build — exit 7\n",
                        Vec::new(),
                        b"::endgroup::\n".as_slice(),
                    )
                } else {
                    ("\n", "build — exit 7\n".as_bytes().to_vec(), b"".as_slice())
                };
                expected.extend_from_slice(input);
                if expected.last().is_some_and(|byte| *byte != b'\n') {
                    expected.push(b'\n');
                }
                let (body, rest) = suspended_body(output.strip_prefix(header.as_bytes()).unwrap());
                assert_eq!(body, expected);
                assert_eq!(rest, tail);
            }
        }
    }

    #[test]
    fn a_spool_read_error_still_resumes_commands_and_closes_the_group() {
        struct ReadError;

        impl Read for ReadError {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("spool read failed"))
            }
        }

        let capture = capture("build", FailureReplay::Grouped);
        let mut input = b"partial".as_slice().chain(ReadError);
        let mut output = Vec::new();
        let error = actions_rs::log::group_to("build", &mut output, |group| {
            capture.write_replay(&mut input, None, group)
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "spool read failed");
        let (body, tail) = suspended_body(output.strip_prefix(b"::group::build\n").unwrap());
        assert_eq!(body, b"partial\n");
        assert_eq!(tail, b"::endgroup::\n");
    }
}
