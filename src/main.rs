//! Universal project task runner.
//!
//! `runner` auto-detects your project's toolchain (package managers, task
//! runners, version constraints) and provides a unified interface to run tasks,
//! install dependencies, clean artifacts, and execute ad-hoc commands.
//!
//! # Supported ecosystems
//!
//! **Package managers:** npm, yarn, pnpm, bun, cargo, deno, uv, poetry,
//! pipenv, go, bundler, composer
//!
//! **Task runners:** turbo, nx, make, just, go-task, mise
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

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::bare_urls)]
#![deny(rustdoc::invalid_codeblock_attributes)]
#![deny(rustdoc::invalid_html_tags)]
#![deny(rustdoc::invalid_rust_codeblocks)]
#![warn(rustdoc::redundant_explicit_links)]
#![warn(rustdoc::unescaped_backticks)]
#![warn(missing_docs)]

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
    let ctx = detect::detect(&cwd)?;

    let code = match cli.command {
        None | Some(cli::Command::Info) => return cmd::info(&ctx),
        Some(cli::Command::Run { task, args }) => cmd::run(&ctx, &task, &args)?,
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                return cmd::info(&ctx);
            }
            cmd::run(&ctx, &args[0], &args[1..])?
        }
        Some(cli::Command::Install { frozen }) => return cmd::install(&ctx, frozen),
        Some(cli::Command::Clean { yes }) => return cmd::clean(&ctx, yes),
        Some(cli::Command::List { raw }) => return cmd::list(&ctx, raw),
        Some(cli::Command::Exec { args }) => cmd::exec(&ctx, &args)?,
        Some(cli::Command::Completions { shell }) => return cmd::completions(shell),
    };

    std::process::exit(code);
}
