//! Alias binary entrypoint for the `run` CLI shim.

use anyhow::Result;

/// Entry point.
fn main() -> Result<()> {
    let code = runner::run_from_env()?;
    std::process::exit(code);
}
