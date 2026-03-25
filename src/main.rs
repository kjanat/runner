mod cli;
mod detect;
mod exec;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let cwd = std::env::current_dir()?;
    let ctx = detect::detect(&cwd)?;

    match cli.command {
        None | Some(cli::Command::Info) => exec::info(&ctx),
        Some(cli::Command::Run { task, args }) => exec::run(&ctx, &task, &args),
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                exec::info(&ctx)
            } else {
                exec::run(&ctx, &args[0], &args[1..])
            }
        }
        Some(cli::Command::Install { frozen }) => exec::install(&ctx, frozen),
        Some(cli::Command::Clean { yes }) => exec::clean(&ctx, yes),
        Some(cli::Command::List { raw }) => exec::list(&ctx, raw),
        Some(cli::Command::Exec { args }) => exec::exec(&ctx, &args),
        Some(cli::Command::Completions { shell }) => exec::completions(shell),
    }
}
