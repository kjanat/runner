//! Binary entrypoint for the `runner` CLI.
//!
//! Library docs live in `src/lib.rs`.

use anyhow::Result;

/// Entry point.
fn main() -> Result<()> {
    let code = runner::run_from_env()?;
    std::process::exit(code);
}
