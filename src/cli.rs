//! Command-line interface definition via [`clap`].

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use clap_complete::aot::Shell;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate, SubcommandCandidates};
/// Produce [`CompletionCandidate`]s for every detected task in the current
/// directory. Called lazily by clap's runtime completion engine — only runs
/// when the shell is actually requesting completions, never during normal
/// execution.
fn task_candidates() -> Vec<CompletionCandidate> {
    let Ok(dir) = completion_dir() else {
        return vec![];
    };
    let ctx = crate::detect::detect(&dir);
    task_candidates_from(&ctx.tasks)
}

fn completion_dir() -> std::io::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    Ok(resolve_completion_dir(
        &cwd,
        std::env::var_os("RUNNER_DIR").as_deref(),
    ))
}

fn resolve_completion_dir(cwd: &Path, env_dir: Option<&std::ffi::OsStr>) -> PathBuf {
    match env_dir.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    }
}

/// Build [`CompletionCandidate`]s from a task list.
///
/// When a task name appears in more than one source, both the bare name *and*
/// a `source:name` qualified form are emitted for each occurrence, enabling
/// disambiguation via tab-completion.
fn task_candidates_from(tasks: &[crate::types::Task]) -> Vec<CompletionCandidate> {
    use std::collections::HashMap;

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for task in tasks {
        *counts.entry(&task.name).or_default() += 1;
    }

    let mut candidates = Vec::new();
    let mut seen_bare = std::collections::HashSet::new();
    for task in tasks {
        let help = task.description.as_ref().map_or_else(
            || task.source.label().to_string(),
            |desc| format!("{}: {desc}", task.source.label()),
        );
        let tag = task.source.label();
        let is_duplicate = counts.get(task.name.as_str()).copied().unwrap_or(0) > 1;

        // Emit bare candidate only once (first source wins for the bare name)
        if seen_bare.insert(&task.name) {
            candidates.push(
                CompletionCandidate::new(&task.name)
                    .help(Some(help.clone().into()))
                    .tag(Some(tag.into()))
                    .display_order(Some(usize::from(task.source.display_order()))),
            );
        }

        // For duplicate names, also emit "source:name" qualified form
        if is_duplicate {
            let qualified = format!("{}:{}", task.source.label(), task.name);
            candidates.push(
                CompletionCandidate::new(qualified)
                    .help(Some(help.into()))
                    .tag(Some(tag.into()))
                    .display_order(Some(usize::from(task.source.display_order()))),
            );
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::{resolve_completion_dir, task_candidates_from};
    use crate::types::{Task, TaskSource};

    #[test]
    fn qualified_candidates_emitted_for_duplicates() {
        let tasks = vec![
            Task {
                name: "test".into(),
                source: TaskSource::PackageJson,
                description: None,
            },
            Task {
                name: "test".into(),
                source: TaskSource::Makefile,
                description: None,
            },
            Task {
                name: "build".into(),
                source: TaskSource::PackageJson,
                description: None,
            },
        ];
        let candidates = task_candidates_from(&tasks);
        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();
        // "test" appears as bare (once) + both qualified forms; "build" is bare only
        assert_eq!(
            values.iter().filter(|v| *v == "test").count(),
            1,
            "bare 'test' should appear exactly once"
        );
        assert!(values.contains(&"package.json:test".to_string()));
        assert!(values.contains(&"Makefile:test".to_string()));
        assert!(values.contains(&"build".to_string()));
        assert!(!values.contains(&"package.json:build".to_string()));
    }

    #[test]
    fn resolve_completion_dir_uses_absolute_runner_dir_env() {
        let dir = resolve_completion_dir(
            Path::new("/tmp/workspace"),
            Some(OsStr::new("/tmp/runner-target")),
        );

        assert_eq!(dir, PathBuf::from("/tmp/runner-target"));
    }
}

/// Universal project task runner.
#[derive(Parser)]
#[command(
    name = "runner",
    about = clap::crate_description!(),
    help_template = "{about-with-newline}{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}",
    version,
    arg_required_else_help = false,
    add = SubcommandCandidates::new(task_candidates)
)]
pub(crate) struct Cli {
    /// Use this directory instead of the current one.
    #[arg(
        long = "dir",
        global = true,
        env = "RUNNER_DIR",
        value_name = "PATH",
        value_hint = clap::ValueHint::DirPath,
        value_parser = clap::value_parser!(PathBuf)
    )]
    pub project_dir: Option<PathBuf>,

    /// Subcommand to execute. Defaults to [`Command::Info`] when absent.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run a task, or exec a command through the detected package manager
    #[command(alias = "r")]
    Run {
        /// Task name or command to execute
        #[arg(add = ArgValueCandidates::new(task_candidates))]
        task: String,
        /// Arguments forwarded to the task/command
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

    /// Show detected project info
    Info,

    /// Generate shell completions
    Completions {
        /// Target shell (defaults to $SHELL)
        #[arg(value_parser = clap::value_parser!(Shell))]
        shell: Option<Shell>,
    },

    /// Catch-all: treat unknown subcommands as task names.
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// CLI used by the `run` alias binary. Behaves as a shortcut for
/// `runner run <task>`: the first positional is the task or command,
/// any remaining positionals are forwarded as its arguments, and
/// built-in subcommand names are never parsed specially (so
/// `run foo bar` runs `foo` with `bar`, not two separate targets).
#[derive(Debug, Parser)]
#[command(
    name = "run",
    about = "Run a project task or exec a command through the detected package manager",
    help_template = "{about-with-newline}{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}",
    version,
    arg_required_else_help = false
)]
pub(crate) struct RunAliasCli {
    /// Use this directory instead of the current one.
    #[arg(
        long = "dir",
        global = true,
        env = "RUNNER_DIR",
        value_name = "PATH",
        value_hint = clap::ValueHint::DirPath,
        value_parser = clap::value_parser!(PathBuf)
    )]
    pub project_dir: Option<PathBuf>,

    /// Task name or command. When omitted, prints project info.
    #[arg(add = ArgValueCandidates::new(task_candidates))]
    pub task: Option<String>,

    /// Arguments forwarded to the task/command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
