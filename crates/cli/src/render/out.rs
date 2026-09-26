//! Where a builtin writes.

use std::io::{IsTerminal as _, Write};

/// The streams a builtin writes to.
pub(crate) enum Out<'a> {
    /// The process's own stdout and stderr, with stdin for prompts.
    Stdio(std::io::Stdout, std::io::Stderr),
    /// Buffers a parallel chain replays through its output.
    Captured(&'a mut Vec<u8>, &'a mut Vec<u8>),
}

impl Out<'_> {
    pub(crate) fn stdio() -> Self {
        Self::Stdio(std::io::stdout(), std::io::stderr())
    }

    pub(crate) fn stdout(&mut self) -> &mut dyn Write {
        match self {
            Self::Stdio(stdout, _) => stdout,
            Self::Captured(stdout, _) => *stdout,
        }
    }

    pub(crate) fn stderr(&mut self) -> &mut dyn Write {
        match self {
            Self::Stdio(_, stderr) => stderr,
            Self::Captured(_, stderr) => *stderr,
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Stdio(stdout, _) if stdout.is_terminal())
    }
}
