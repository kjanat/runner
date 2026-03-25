//! # Runner
//!
//! Universal project task runner.
//!
//! `runner` auto-detects your project's toolchain (package managers, task
//! runners, version constraints) and provides a unified interface to run
//! tasks, install dependencies, clean artifacts, and execute ad-hoc commands.
//!
//! # Supported ecosystems
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
//! # Library API
//!
//! - [`run_from_env`] parses process args and dispatches in current dir.
//! - [`run_from_args`] parses explicit args and dispatches in current dir.
//! - [`run_in_dir`] parses explicit args and dispatches against a given dir.
//!
//! # CLI usage
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
/// Returns an error when reading current dir fails, argument parsing fails,
/// project detection fails, or command execution fails.
pub fn run_from_env() -> Result<i32> {
    run_from_args(std::env::args_os())
}

/// Parse explicit args, detect current dir, dispatch, return exit code.
///
/// `args` must include argv[0] as first item.
///
/// # Errors
///
/// Returns an error when reading current dir fails, argument parsing fails,
/// project detection fails, or command execution fails.
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
/// `args` must include argv[0] as first item.
///
/// # Errors
///
/// Returns an error when argument parsing fails, project detection fails,
/// or command execution fails.
pub fn run_in_dir<I, T>(args: I, dir: &Path) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = cli::Cli::parse_from(args);
    dispatch(cli, dir)
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
        Some(cli::Command::Clean { yes }) => {
            cmd::clean(&ctx, yes)?;
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
