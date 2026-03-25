//! `runner completions` — generate shell completion scripts.

use std::io;

use clap::CommandFactory;

/// Write shell completions for the given `shell` to stdout.
pub(crate) fn completions(shell: clap_complete::Shell) {
    clap_complete::generate(
        shell,
        &mut crate::cli::Cli::command(),
        "runner",
        &mut io::stdout(),
    );
}
