//! Command-line interface definition via [`clap`].

use clap::{Parser, Subcommand};
use clap_complete::aot::Shell;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate, SubcommandCandidates};

/// Produce [`CompletionCandidate`]s for every detected task in the current
/// directory. Called lazily by clap's runtime completion engine — only runs
/// when the shell is actually requesting completions, never during normal
/// execution.
fn task_candidates() -> Vec<CompletionCandidate> {
    let Ok(dir) = std::env::current_dir() else {
        return vec![];
    };
    let ctx = crate::detect::detect(&dir);
    ctx.tasks
        .into_iter()
        .map(|task| {
            let help = match task.description {
                Some(desc) => format!("{}: {desc}", task.source.label()),
                None => task.source.label().to_string(),
            };
            CompletionCandidate::new(&task.name)
                .help(Some(help.into()))
                .tag(Some(task.source.label().into()))
                .display_order(Some(usize::from(task.source.display_order())))
        })
        .collect()
}

/// Universal project task runner.
#[derive(Parser)]
#[command(
    name = "runner",
    about = "Universal project task runner",
    version,
    arg_required_else_help = false,
    add = SubcommandCandidates::new(task_candidates)
)]
pub(crate) struct Cli {
    /// Subcommand to execute. Defaults to [`Command::Info`] when absent.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Available subcommands.
#[derive(Subcommand)]
pub(crate) enum Command {
    /// Run a task/script (or just `runner <task>`)
    #[command(alias = "r")]
    Run {
        /// Task name
        #[arg(add = ArgValueCandidates::new(task_candidates))]
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
        /// Include framework-specific Node build dirs like `.next`
        #[arg(long)]
        include_framework: bool,
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

    /// Generate shell completions (auto-detects shell from $SHELL)
    Completions {
        /// Target shell (defaults to $SHELL)
        shell: Option<Shell>,
    },

    /// Catch-all: treat unknown subcommands as task names.
    #[command(external_subcommand)]
    External(Vec<String>),
}
