//! Command-line interface definition via [`clap`].

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use clap_complete::aot::Shell;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate, SubcommandCandidates};

/// Sort aliases after all real recipes in completion candidates by offsetting
/// their display order beyond any realistic [`TaskSource::display_order`] value.
const ALIAS_DISPLAY_ORDER_OFFSET: usize = 100;
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
///
/// Exception: a `package.json` script whose body is a literal turbo
/// passthrough wrapper (`"build": "turbo run build"`, the canonical
/// Turborepo pattern) is dropped from completion candidates *iff* a
/// same-named `turbo.json` task also exists. The passthrough flag is set
/// during detection by inspecting the actual script body
/// ([`crate::tool::turbo::is_self_passthrough`]), so a real script like
/// `"build": "vite build"` keeps its qualified form even when a
/// `turbo.json` `build` task is present. `runner list` still surfaces both
/// sources for transparency, and `runner build` already dispatches through
/// turbo per the source-priority order in `cmd::run::source_priority`.
fn task_candidates_from(tasks: &[crate::types::Task]) -> Vec<CompletionCandidate> {
    use std::collections::{HashMap, HashSet};

    use crate::types::TaskSource;

    let mut sources_for_name: HashMap<&str, HashSet<TaskSource>> = HashMap::new();
    for task in tasks {
        sources_for_name
            .entry(&task.name)
            .or_default()
            .insert(task.source);
    }

    // A `package.json` script is only swallowed when it (a) declared itself a
    // turbo passthrough at detection time *and* (b) the project actually has a
    // same-named `turbo.json` task to absorb it. Without (b), suppressing
    // would leave the user with no completion for the script at all.
    let is_turbo_passthrough = |task: &crate::types::Task| -> bool {
        task.passthrough_to_turbo
            && task.source == TaskSource::PackageJson
            && sources_for_name
                .get(task.name.as_str())
                .is_some_and(|set| set.contains(&TaskSource::TurboJson))
    };

    let mut effective_count: HashMap<&str, usize> = HashMap::new();
    for task in tasks {
        if !is_turbo_passthrough(task) {
            *effective_count.entry(task.name.as_str()).or_default() += 1;
        }
    }

    let mut candidates = Vec::new();
    let mut seen_bare = HashSet::new();
    for task in tasks {
        if is_turbo_passthrough(task) {
            continue;
        }

        let source_label = task.source.label();
        // Separate tag group keeps aliases under their own zsh section instead
        // of interleaving with real recipes.
        let (help, tag, order) = task.alias_of.as_deref().map_or_else(
            || {
                let help = task.description.as_ref().map_or_else(
                    || source_label.to_string(),
                    |desc| format!("{source_label}: {desc}"),
                );
                (
                    help,
                    source_label.to_string(),
                    usize::from(task.source.display_order()),
                )
            },
            |target| {
                let help = format!("→ {target}");
                let tag = format!("{source_label} (aliases)");
                let order = usize::from(task.source.display_order()) + ALIAS_DISPLAY_ORDER_OFFSET;
                (help, tag, order)
            },
        );
        let is_duplicate = effective_count
            .get(task.name.as_str())
            .copied()
            .unwrap_or(0)
            > 1;

        // Emit bare candidate only once (first source wins for the bare name)
        if seen_bare.insert(&task.name) {
            candidates.push(
                CompletionCandidate::new(&task.name)
                    .help(Some(help.clone().into()))
                    .tag(Some(tag.clone().into()))
                    .display_order(Some(order)),
            );
        }

        // For duplicate names, also emit "source:name" qualified form
        if is_duplicate {
            let qualified = format!("{source_label}:{}", task.name);
            candidates.push(
                CompletionCandidate::new(qualified)
                    .help(Some(help.into()))
                    .tag(Some(tag.into()))
                    .display_order(Some(order)),
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

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.into(),
            source,
            description: None,
            alias_of: None,
            passthrough_to_turbo: false,
        }
    }

    fn turbo_passthrough(name: &str) -> Task {
        Task {
            passthrough_to_turbo: true,
            ..task(name, TaskSource::PackageJson)
        }
    }

    #[test]
    fn qualified_candidates_emitted_for_duplicates() {
        let tasks = vec![
            task("test", TaskSource::PackageJson),
            task("test", TaskSource::Makefile),
            task("build", TaskSource::PackageJson),
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
    fn package_json_passthrough_to_turbo_collapses_to_bare_name() {
        let tasks = vec![
            turbo_passthrough("build"),
            task("build", TaskSource::TurboJson),
            task("fmt", TaskSource::PackageJson),
        ];
        let candidates = task_candidates_from(&tasks);
        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            values.iter().filter(|v| *v == "build").count(),
            1,
            "bare 'build' should appear exactly once"
        );
        assert!(
            !values.contains(&"package.json:build".to_string()),
            "the package.json passthrough should not surface a qualified form"
        );
        assert!(
            !values.contains(&"turbo.json:build".to_string()),
            "with the package.json source swallowed, no qualified form is needed"
        );
        assert!(values.contains(&"fmt".to_string()));
    }

    #[test]
    fn passthrough_swallow_keeps_unrelated_runner_qualified_forms() {
        let tasks = vec![
            turbo_passthrough("build"),
            task("build", TaskSource::Makefile),
            task("build", TaskSource::TurboJson),
        ];
        let candidates = task_candidates_from(&tasks);
        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();

        assert!(values.contains(&"build".to_string()));
        assert!(
            !values.contains(&"package.json:build".to_string()),
            "package.json must remain swallowed even when other runners share the name"
        );
        assert!(
            values.contains(&"Makefile:build".to_string()),
            "Makefile is a real definition, not a passthrough — keep its qualified form"
        );
        assert!(
            values.contains(&"turbo.json:build".to_string()),
            "turbo.json must keep a qualified form to disambiguate from Makefile"
        );
    }

    #[test]
    fn real_package_json_script_keeps_qualified_form_alongside_turbo() {
        // Regression guard: a real `"build": "vite build"` script that
        // happens to share its name with a `turbo.json` task must NOT be
        // swallowed — the passthrough flag is set per-script-body during
        // detection, not inferred from name collisions alone.
        let tasks = vec![
            // Same name, but `passthrough_to_turbo: false` because the
            // command body is `vite build`, not `turbo run build`.
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::TurboJson),
        ];
        let candidates = task_candidates_from(&tasks);
        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();

        assert!(values.contains(&"build".to_string()));
        assert!(
            values.contains(&"package.json:build".to_string()),
            "a real package.json script must surface its qualified form for disambiguation"
        );
        assert!(
            values.contains(&"turbo.json:build".to_string()),
            "the turbo.json source must surface its qualified form when a real twin exists"
        );
    }

    #[test]
    fn passthrough_without_turbo_twin_stays_visible() {
        // Misconfigured project: `"build": "turbo run build"` but no
        // `turbo.json` to back it. Suppressing here would leave the user
        // with no completion at all, so the passthrough must remain.
        let tasks = vec![turbo_passthrough("build")];
        let candidates = task_candidates_from(&tasks);

        assert!(
            candidates
                .iter()
                .any(|c| c.get_value().to_string_lossy() == "build"),
            "without a turbo.json twin, the passthrough is the only source — keep it"
        );
    }

    #[test]
    fn alias_candidate_uses_arrow_help_and_dedicated_tag() {
        let tasks = vec![
            Task {
                description: Some("Build the project".into()),
                ..task("build", TaskSource::Justfile)
            },
            Task {
                alias_of: Some("build".into()),
                ..task("b", TaskSource::Justfile)
            },
        ];
        let candidates = task_candidates_from(&tasks);
        let alias = candidates
            .iter()
            .find(|c| c.get_value() == "b")
            .expect("alias candidate b should be emitted");
        let help = alias
            .get_help()
            .expect("alias candidate should carry help text")
            .to_string();
        assert_eq!(help, "→ build");
        let tag = alias
            .get_tag()
            .expect("alias candidate should carry a tag")
            .to_string();
        assert_eq!(tag, "justfile (aliases)");

        let recipe = candidates
            .iter()
            .find(|c| c.get_value() == "build")
            .expect("recipe candidate build should be emitted");
        let recipe_tag = recipe
            .get_tag()
            .expect("recipe candidate should carry a tag")
            .to_string();
        assert_eq!(recipe_tag, "justfile");
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

    /// Override the detected package manager (e.g. `pnpm`, `bun`, `yarn`).
    /// Falls back to `$RUNNER_PM` when omitted.
    #[arg(long = "pm", global = true, value_name = "NAME")]
    pub pm_override: Option<String>,

    /// Override the detected task runner (e.g. `just`, `turbo`, `make`).
    /// Falls back to `$RUNNER_RUNNER` when omitted.
    #[arg(long = "runner", global = true, value_name = "NAME")]
    pub runner_override: Option<String>,

    /// What to do when no detection signal matches: `probe` (default,
    /// PATH probe), `npm` (legacy silent fallback), `error` (refuse).
    /// Falls back to `$RUNNER_FALLBACK` when omitted.
    #[arg(long = "fallback", global = true, value_name = "POLICY")]
    pub fallback: Option<String>,

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
        /// Target shell — bare name (`zsh`) or full path (`/usr/bin/zsh`).
        /// Defaults to `$SHELL`.
        #[arg(value_parser = crate::cmd::parse_shell_arg)]
        shell: Option<Shell>,

        /// Write the completion script to <PATH> instead of stdout. Any
        /// existing file is overwritten.
        #[arg(
            short = 'o',
            long = "output",
            value_name = "PATH",
            value_hint = clap::ValueHint::FilePath,
            value_parser = clap::value_parser!(PathBuf),
        )]
        output: Option<PathBuf>,
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

    /// Override the detected package manager (e.g. `pnpm`, `bun`, `yarn`).
    /// Falls back to `$RUNNER_PM` when omitted.
    #[arg(long = "pm", global = true, value_name = "NAME")]
    pub pm_override: Option<String>,

    /// Override the detected task runner (e.g. `just`, `turbo`, `make`).
    /// Falls back to `$RUNNER_RUNNER` when omitted.
    #[arg(long = "runner", global = true, value_name = "NAME")]
    pub runner_override: Option<String>,

    /// What to do when no detection signal matches: `probe` (default,
    /// PATH probe), `npm` (legacy silent fallback), `error` (refuse).
    /// Falls back to `$RUNNER_FALLBACK` when omitted.
    #[arg(long = "fallback", global = true, value_name = "POLICY")]
    pub fallback: Option<String>,

    /// Task name or command. When omitted, prints project info.
    #[arg(add = ArgValueCandidates::new(task_candidates))]
    pub task: Option<String>,

    /// Arguments forwarded to the task/command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
