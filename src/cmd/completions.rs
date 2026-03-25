use std::io;

use anyhow::Result;
use clap::CommandFactory;

pub fn completions(shell: clap_complete::Shell) -> Result<()> {
    clap_complete::generate(
        shell,
        &mut crate::cli::Cli::command(),
        "runner",
        &mut io::stdout(),
    );
    Ok(())
}
