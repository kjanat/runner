//! Alias binary entrypoint for the `run` CLI shim. Treats every positional
//! argument as a task or command routed through the unified `runner run`
//! path, so built-in subcommand names (`clean`, `install`, …) don't shadow
//! same-named tasks.

/// Entry point. See [`crate::main`] in `runner` for the matching
/// exit-code mapping rationale.
fn main() {
    let code = match runner::run_alias_from_env() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err:#}");
            runner::exit_code_for_error(&err)
        }
    };
    std::process::exit(code);
}
