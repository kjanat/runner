//! # Runner
//!
//! ## Overview
//!
//! Universal project task runner.
//!
//! `runner` auto-detects your project's toolchain (package managers, task
//! runners, version constraints) and provides a unified interface to run
//! tasks, install dependencies, clean artifacts, and execute ad-hoc commands.
//!
//! ## Supported Ecosystems
//!
//! **Package managers/ecosystems:** [npm], [yarn], [pnpm], [bun], [cargo],
//! [deno], [uv], [poetry], [pipenv], [go], [bundler], [composer]
//!
//! **Task runners:** [turbo], [nx], [make], [just], [go-task], [mise]
//!
//! [npm]: https://www.npmjs.com/
//! [yarn]: https://yarnpkg.com/
//! [pnpm]: https://pnpm.io/
//! [bun]: https://bun.sh/
//! [cargo]: https://doc.rust-lang.org/cargo/
//! [deno]: https://deno.land/
//! [uv]: https://github.com/astral-sh/uv/
//! [poetry]: https://python-poetry.org/
//! [pipenv]: https://pipenv.pypa.io/
//! [go]: https://go.dev/
//! [bundler]: https://bundler.io/
//! [composer]: https://getcomposer.org/
//! [turbo]: https://turborepo.dev/
//! [nx]: https://nx.dev/
//! [make]: https://www.gnu.org/software/make/
//! [just]: https://just.systems/
//! [go-task]: https://taskfile.dev/
//! [mise]: https://mise.jdx.dev/
//!
//! ## Library API
//!
//! - [`run_from_env`] parses process args and dispatches in current dir.
//! - [`run_from_args`] parses explicit args and dispatches in current dir.
//! - [`run_in_dir`] parses explicit args and dispatches against a given dir.
//!
//! ## CLI Usage
//!
//! ```text
//! runner              # show detected project info
//! runner <task>       # run a task (auto-routed to the right tool)
//! runner install      # install dependencies via detected PM
//! runner clean        # remove caches and build artifacts
//! runner list         # list available tasks from all sources
//! runner exec <cmd>   # run a command through the package manager
//! ```
//!
//! Generate docs with `cargo doc --document-private-items --open`.

mod cli;
mod cmd;
mod detect;
mod tool;
mod types;

use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;
use clap::Parser;

/// Parse process args, detect current dir, dispatch, return exit code.
///
/// # Errors
///
/// Returns an error when reading current dir fails, project detection fails,
/// command execution fails, or writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_from_env() -> Result<i32> {
    run_from_args(std::env::args_os())
}

/// Parse explicit args, detect current dir, dispatch, return exit code.
///
/// `args` must include `argv\[0\]` as first item.
///
/// # Errors
///
/// Returns an error when reading current dir fails, project detection fails,
/// command execution fails, or writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_from_args<I, T>(args: I) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cwd = std::env::current_dir()?;
    run_in_dir(args, &cwd)
}

/// Parse explicit args and run against `dir`.
///
/// `args` must include `argv\[0\]` as first item.
///
/// # Errors
///
/// Returns an error when project detection fails, command execution fails, or
/// writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_in_dir<I, T>(args: I, dir: &Path) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match cli::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return render_clap_error(&err),
    };
    dispatch(cli, dir)
}

fn render_clap_error(err: &clap::Error) -> Result<i32> {
    let exit_code = err.exit_code();
    err.print()?;
    Ok(exit_code)
}

fn dispatch(cli: cli::Cli, dir: &Path) -> Result<i32> {
    let ctx = detect::detect(dir);

    match cli.command {
        None | Some(cli::Command::Info) => {
            cmd::info(&ctx);
            Ok(0)
        }
        Some(cli::Command::Run { task, args }) => cmd::run(&ctx, &task, &args),
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                cmd::info(&ctx);
                Ok(0)
            } else {
                cmd::run(&ctx, &args[0], &args[1..])
            }
        }
        Some(cli::Command::Install { frozen }) => {
            cmd::install(&ctx, frozen)?;
            Ok(0)
        }
        Some(cli::Command::Clean {
            yes,
            include_framework,
        }) => {
            cmd::clean(&ctx, yes, include_framework)?;
            Ok(0)
        }
        Some(cli::Command::List { raw }) => {
            cmd::list(&ctx, raw);
            Ok(0)
        }
        Some(cli::Command::Exec { args }) => cmd::exec(&ctx, &args),
        Some(cli::Command::Completions { shell }) => {
            cmd::completions(shell);
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::run_in_dir;

    #[test]
    fn help_returns_zero_instead_of_exiting() {
        let code = run_in_dir(["runner", "--help"], Path::new("."))
            .expect("help should return an exit code");

        assert_eq!(code, 0);
    }

    #[test]
    fn invalid_args_return_non_zero_instead_of_exiting() {
        let code = run_in_dir(["runner", "--definitely-invalid"], Path::new("."))
            .expect("parse errors should return an exit code");

        assert_ne!(code, 0);
    }

    #[test]
    fn version_returns_zero_instead_of_exiting() {
        let code = run_in_dir(["runner", "--version"], Path::new("."))
            .expect("version should return an exit code");

        assert_eq!(code, 0);
    }
}
