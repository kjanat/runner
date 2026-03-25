mod cli;
mod cmd;
mod detect;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let cwd = std::env::current_dir()?;
    let ctx = detect::detect(&cwd)?;

    match cli.command {
        None | Some(cli::Command::Info) => cmd::info(&ctx),
        Some(cli::Command::Run { task, args }) => cmd::run(&ctx, &task, &args),
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                cmd::info(&ctx)
            } else {
                cmd::run(&ctx, &args[0], &args[1..])
            }
        }
        Some(cli::Command::Install { frozen }) => cmd::install(&ctx, frozen),
        Some(cli::Command::Clean { yes }) => cmd::clean(&ctx, yes),
        Some(cli::Command::List { raw }) => cmd::list(&ctx, raw),
        Some(cli::Command::Exec { args }) => cmd::exec(&ctx, &args),
        Some(cli::Command::Completions { shell }) => cmd::completions(shell),
    }
}
