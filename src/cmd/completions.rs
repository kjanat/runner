//! `runner completions` — generate shell completion scripts.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use clap::CommandFactory;

/// Write shell completions for the given `shell` to stdout.
pub(crate) fn completions(shell: clap_complete::Shell) {
    let bin_name = completion_bin_name(std::env::args_os().next());

    clap_complete::generate(
        shell,
        &mut crate::cli::Cli::command(),
        bin_name,
        &mut io::stdout(),
    );
}

fn completion_bin_name(arg0: Option<OsString>) -> String {
    arg0.and_then(|raw| {
        Path::new(&raw)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
    .filter(|name| !name.is_empty())
    .unwrap_or_else(|| "runner".to_string())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::completion_bin_name;

    #[test]
    fn completion_bin_name_uses_file_name_from_path() {
        assert_eq!(completion_bin_name(Some(OsString::from("/tmp/run"))), "run");
    }

    #[test]
    fn completion_bin_name_falls_back_to_runner_when_missing() {
        assert_eq!(completion_bin_name(None), "runner");
    }

    #[test]
    fn completion_bin_name_falls_back_to_runner_when_empty() {
        assert_eq!(completion_bin_name(Some(OsString::from(""))), "runner");
    }
}
