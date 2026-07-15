//! Binary entrypoint for the `runner` CLI.
//!
//! Library docs live in `src/lib.rs`.

/// Entry point.
///
/// Maps a returned [`anyhow::Error`] to the right exit code via
/// [`runner::exit_code_for_error`], `ResolveError` → 2, everything else
/// → 1. The default `Termination` impl on `anyhow::Error` collapses
/// every failure to 1 with the same printed format, which is what was
/// happening before this binary started caring about resolver-specific
/// failures.
fn main() {
    let code = match runner::run_from_env() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err:#}");
            runner::exit_code_for_error(&err)
        }
    };
    std::process::exit(code);
}
