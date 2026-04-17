//! Alias binary entrypoint for the `run` CLI shim. Treats every positional
//! argument as a task or command routed through the unified `runner run`
//! path, so built-in subcommand names (`clean`, `install`, …) don't shadow
//! same-named tasks.

use anyhow::Result;

/// Entry point.
fn main() -> Result<()> {
    let code = runner::run_alias_from_env()?;
    std::process::exit(code);
}
