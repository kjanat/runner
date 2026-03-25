//! # Runner
//!
//! > Universal project task runner.
//!
//! `runner` auto-detects your project's toolchain (package managers, task
//! runners, version constraints) and provides a unified interface to run tasks,
//! install dependencies, clean artifacts, and execute ad-hoc commands.
//!
//! # Supported ecosystems
//!
//! **Package managers/ecosystems:** [npm], [yarn], [pnpm], [bun], [cargo], [deno], [uv], [poetry],
//! [pipenv], [go], [bundler], [composer]
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
//! # Usage
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

use anyhow::Result;
use clap::Parser;

/// Entry point. Parses CLI args, detects the project context, dispatches.
fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let cwd = std::env::current_dir()?;
    let ctx = detect::detect(&cwd);

    let code = match cli.command {
        None | Some(cli::Command::Info) => {
            cmd::info(&ctx);
            return Ok(());
        }
        Some(cli::Command::Run { task, args }) => cmd::run(&ctx, &task, &args)?,
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                cmd::info(&ctx);
                return Ok(());
            }
            cmd::run(&ctx, &args[0], &args[1..])?
        }
        Some(cli::Command::Install { frozen }) => return cmd::install(&ctx, frozen),
        Some(cli::Command::Clean { yes }) => return cmd::clean(&ctx, yes),
        Some(cli::Command::List { raw }) => {
            cmd::list(&ctx, raw);
            return Ok(());
        }
        Some(cli::Command::Exec { args }) => cmd::exec(&ctx, &args)?,
        Some(cli::Command::Completions { shell }) => {
            cmd::completions(shell);
            return Ok(());
        }
    };

    std::process::exit(code);
}
