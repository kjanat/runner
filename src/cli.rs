use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "runner",
    about = "Universal project task runner",
    version,
    arg_required_else_help = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a task/script (or just `runner <task>`)
    #[command(alias = "r")]
    Run {
        /// Task name
        task: String,
        /// Arguments forwarded to the task
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Install project dependencies
    #[command(alias = "i")]
    Install {
        /// Reproducible install from lockfile (npm ci, --frozen-lockfile, etc.)
        #[arg(long)]
        frozen: bool,
    },

    /// Remove caches and build artifacts
    Clean {
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// List available tasks across all detected sources
    #[command(alias = "ls")]
    List {
        /// Print bare task names, one per line (for scripting / completions)
        #[arg(long)]
        raw: bool,
    },

    /// Execute a command through the detected package manager
    #[command(alias = "x")]
    Exec {
        /// Command and arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },

    /// Show detected project info
    Info,

    /// Generate shell completions
    Completions {
        /// Target shell
        shell: Shell,
    },

    /// (hidden) catch-all — treat unknown subcommands as task names
    #[command(external_subcommand)]
    External(Vec<String>),
}
