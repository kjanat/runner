//! Command-line interface definition via [`clap`].

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::builder::styling::{AnsiColor, Color, Style, Styles};
use clap::{Args, Parser, Subcommand};
use clap_complete::aot::Shell;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate, SubcommandCandidates};

use crate::types::{PackageManager, TaskRunner};

/// Color palette for help output. clap auto-disables when stdout isn't a
/// TTY or `NO_COLOR` is set, so the same constant works for piped output
/// and color-averse users without extra plumbing.
const HELP_STYLES: Styles = Styles::styled()
    .header(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
            .bold()
            .underline(),
    )
    .usage(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
            .bold()
            .underline(),
    )
    .literal(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
            .bold(),
    )
    .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
    .valid(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Green)))
            .bold(),
    )
    .invalid(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Red)))
            .bold(),
    )
    .error(
        Style::new()
            .fg_color(Some(Color::Ansi(AnsiColor::Red)))
            .bold(),
    );

/// ANSI cyan wrapper used for inline literals embedded in flag-help prose
/// (PM names, env-var names, etc.). The `HELP_STYLES` `Styles::literal` /
/// `Styles::placeholder` slots only style structural pieces (flag names,
/// value placeholders); for tokens inside the description body we emit
/// ANSI directly. clap routes its output through `anstream`, which strips
/// ANSI when stdout isn't a TTY or `NO_COLOR` is set, so these inline
/// escapes are dropped automatically for piped output.
macro_rules! cyan {
    ($s:literal) => {
        concat!("\x1b[36m", $s, "\x1b[0m")
    };
}

/// Wrap a runtime string in the same cyan ANSI escape pair the [`cyan!`] macro
/// emits for compile-time literals. clap routes help through `anstream`, which
/// strips ANSI on non-TTY / `NO_COLOR` output.
fn cyan_str(s: &str) -> String {
    format!("\x1b[36m{s}\x1b[0m")
}

/// Comma-joined, cyan-styled list of every [`PackageManager`] label, with the
/// `bundle` alias for `bundler` called out so users discover both spellings.
/// Built once at first help-text access via [`LazyLock`]; rebuilding the
/// list on every `--help` invocation would waste work for a value that is
/// fully determined by the [`PackageManager::all`] enumeration.
static PM_HELP: LazyLock<String> = LazyLock::new(|| {
    let joined = PackageManager::all()
        .iter()
        .map(|pm| {
            if matches!(pm, PackageManager::Bundler) {
                format!("{} (alias: bundle)", pm.label())
            } else {
                pm.label().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Override the detected package manager (also reads {} when omitted). Valid: {joined}",
        cyan_str("RUNNER_PM"),
    )
});

/// Comma-joined, cyan-styled list of every [`TaskRunner`] label, with the
/// `go-task` alias for `task` called out. Lazy-built for the same reason as
/// [`PM_HELP`].
static RUNNER_HELP: LazyLock<String> = LazyLock::new(|| {
    let joined = TaskRunner::all()
        .iter()
        .map(|r| {
            if matches!(r, TaskRunner::GoTask) {
                format!("{} (alias: go-task)", r.label())
            } else {
                r.label().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Override the detected task runner (also reads {} when omitted). Valid: {joined}",
        cyan_str("RUNNER_RUNNER"),
    )
});

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
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    Ok(resolve_completion_dir(
        &cwd,
        cli_dir_from_argv(&argv).as_deref(),
        std::env::var_os("RUNNER_DIR").as_deref(),
    ))
}

/// Mirror clap's `--dir` precedence at completion time.
///
/// Precedence (highest first) — same as the resolver at runtime so
/// the completion list matches the directory the user is about to
/// dispatch against:
/// 1. `--dir` parsed from the in-flight argv (the user is typing
///    `runner --dir /other/repo <TAB>`).
/// 2. `RUNNER_DIR` env var.
/// 3. The shell's working directory.
fn resolve_completion_dir(
    cwd: &Path,
    cli_dir: Option<&std::ffi::OsStr>,
    env_dir: Option<&std::ffi::OsStr>,
) -> PathBuf {
    let raw = cli_dir.or(env_dir);
    match raw.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    }
}

/// Scan the argv (as the shell passed it to the binary during
/// completion) for `--dir <value>` / `--dir=<value>`. Returns the last
/// occurrence so repeated flags behave the same way clap does at parse
/// time.
///
/// `clap_complete`'s bash registration invokes the binary as
/// `completer -- "${words[@]}"`, so the user-typed words live *after*
/// the first `--` separator. We seek past that separator first, then
/// scan; if no separator exists (binary invoked directly without
/// `clap_complete`'s harness, e.g. in tests), the entire tail is
/// scanned.
fn cli_dir_from_argv(argv: &[std::ffi::OsString]) -> Option<std::ffi::OsString> {
    use std::ffi::OsString;

    // Find the `--` separator clap_complete inserts between the
    // completer path and the user's word list. Skip past it; otherwise
    // start at index 1 (after argv[0]).
    let start = argv.iter().position(|a| a == "--").map_or(1, |idx| idx + 1);
    if start >= argv.len() {
        return None;
    }

    let mut found: Option<OsString> = None;
    let mut iter = argv[start..].iter();
    while let Some(arg) = iter.next() {
        if arg == "--dir" {
            if let Some(next) = iter.next() {
                found = Some(next.clone());
            }
            continue;
        }
        if let Some(rest) = arg
            .to_str()
            .and_then(|s| s.strip_prefix("--dir="))
            .map(OsString::from)
        {
            found = Some(rest);
        }
    }
    found
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
    // passthrough wrapper at detection time *and* (b) the project actually
    // has a same-named task from that runner's source to absorb it. Without
    // (b), suppressing would leave the user with no completion for the
    // script at all.
    let is_self_passthrough = |task: &crate::types::Task| -> bool {
        let Some(runner) = task.passthrough_to else {
            return false;
        };
        let Some(peer_source) = runner.task_source() else {
            return false;
        };
        task.source == TaskSource::PackageJson
            && sources_for_name
                .get(task.name.as_str())
                .is_some_and(|set| set.contains(&peer_source))
    };

    let mut effective_count: HashMap<&str, usize> = HashMap::new();
    for task in tasks {
        if !is_self_passthrough(task) {
            *effective_count.entry(task.name.as_str()).or_default() += 1;
        }
    }

    let mut candidates = Vec::new();
    let mut seen_bare = HashSet::new();
    for task in tasks {
        if is_self_passthrough(task) {
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

    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};

    use super::{
        Cli, Command, RunAliasCli, cli_dir_from_argv, resolve_completion_dir, task_candidates_from,
    };
    use crate::types::{Task, TaskSource};

    fn task(name: &str, source: TaskSource) -> Task {
        Task {
            name: name.into(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
        }
    }

    fn turbo_passthrough(name: &str) -> Task {
        Task {
            passthrough_to: Some(crate::types::TaskRunner::Turbo),
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
        assert!(values.contains(&"make:test".to_string()));
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
            values.contains(&"make:build".to_string()),
            "Makefile is a real definition, not a passthrough — keep its qualified form"
        );
        assert!(
            values.contains(&"turbo:build".to_string()),
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
            values.contains(&"turbo:build".to_string()),
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
        assert_eq!(tag, "just (aliases)");

        let recipe = candidates
            .iter()
            .find(|c| c.get_value() == "build")
            .expect("recipe candidate build should be emitted");
        let recipe_tag = recipe
            .get_tag()
            .expect("recipe candidate should carry a tag")
            .to_string();
        assert_eq!(recipe_tag, "just");
    }

    #[test]
    fn resolve_completion_dir_uses_absolute_runner_dir_env() {
        let dir = resolve_completion_dir(
            Path::new("/tmp/workspace"),
            None,
            Some(OsStr::new("/tmp/runner-target")),
        );

        assert_eq!(dir, PathBuf::from("/tmp/runner-target"));
    }

    #[test]
    fn resolve_completion_dir_prefers_cli_over_env() {
        // `runner --dir /cli-target <TAB>` with `RUNNER_DIR=/env-target`
        // set in the environment — completion should reflect the CLI
        // flag, matching clap's runtime precedence.
        let dir = resolve_completion_dir(
            Path::new("/tmp/workspace"),
            Some(OsStr::new("/cli-target")),
            Some(OsStr::new("/env-target")),
        );

        assert_eq!(dir, PathBuf::from("/cli-target"));
    }

    #[test]
    fn cli_dir_from_argv_parses_space_separated_form_with_clap_complete_harness() {
        // Bash completion invokes the binary as
        // `completer -- "${words[@]}"`, so the user-typed words live
        // *after* the first `--`. The helper has to seek past it and
        // then scan for `--dir`.
        let argv = vec![
            OsString::from("/path/to/runner"),
            OsString::from("--"),
            OsString::from("runner"),
            OsString::from("--dir"),
            OsString::from("/repo"),
            OsString::from("build"),
            OsString::from(""),
        ];

        assert_eq!(
            cli_dir_from_argv(&argv).as_deref(),
            Some(OsStr::new("/repo"))
        );
    }

    #[test]
    fn cli_dir_from_argv_parses_space_separated_form_without_separator() {
        // Direct invocation (no clap_complete harness, e.g. tests):
        // scan the full tail starting at argv[1].
        let argv = vec![
            OsString::from("runner"),
            OsString::from("--dir"),
            OsString::from("/repo"),
            OsString::from("build"),
        ];

        assert_eq!(
            cli_dir_from_argv(&argv).as_deref(),
            Some(OsStr::new("/repo"))
        );
    }

    #[test]
    fn cli_dir_from_argv_parses_equals_form() {
        let argv = vec![
            OsString::from("runner"),
            OsString::from("--dir=/repo"),
            OsString::from("build"),
        ];

        assert_eq!(
            cli_dir_from_argv(&argv).as_deref(),
            Some(OsStr::new("/repo"))
        );
    }

    #[test]
    fn cli_dir_from_argv_last_occurrence_wins() {
        // Match clap's behavior for repeated flags: last value wins.
        let argv = vec![
            OsString::from("runner"),
            OsString::from("--dir"),
            OsString::from("/first"),
            OsString::from("--dir=/second"),
        ];

        assert_eq!(
            cli_dir_from_argv(&argv).as_deref(),
            Some(OsStr::new("/second"))
        );
    }

    #[test]
    fn cli_dir_from_argv_returns_none_without_flag() {
        let argv = vec![OsString::from("runner"), OsString::from("build")];

        assert_eq!(cli_dir_from_argv(&argv), None);
    }

    #[test]
    fn run_accepts_sequential_chain_flag() {
        let cli = Cli::try_parse_from(["runner", "run", "-s", "build", "test"]).expect("parses");
        let Some(Command::Run {
            task, args, mode, ..
        }) = cli.command
        else {
            panic!("expected Run subcommand");
        };
        assert!(mode.sequential, "-s should set sequential");
        assert!(!mode.parallel, "-p should not be set");
        assert_eq!(task.as_deref(), Some("build"));
        assert_eq!(args, vec!["test".to_string()]);
    }

    #[test]
    fn run_rejects_sequential_and_parallel_together() {
        let err =
            Cli::try_parse_from(["runner", "run", "-s", "-p", "build"]).expect_err("conflict");
        let msg = format!("{err}");
        assert!(msg.contains("--parallel") || msg.contains("--sequential"));
    }

    #[test]
    fn run_rejects_keep_going_and_kill_on_fail_together() {
        let err = Cli::try_parse_from([
            "runner",
            "run",
            "-s",
            "-k",
            "--kill-on-fail",
            "build",
            "test",
        ])
        .expect_err("conflict");
        let msg = format!("{err}");
        assert!(msg.contains("--keep-going") || msg.contains("--kill-on-fail"));
    }

    #[test]
    fn list_rejects_conflicting_output_modes() {
        let err = Cli::try_parse_from(["runner", "list", "--raw", "--json"])
            .expect_err("list output modes must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn info_subcommand_still_parses_but_is_hidden() {
        // Deprecated alias — must keep parsing (with and without --json)
        // so existing `runner info` invocations don't break …
        Cli::try_parse_from(["runner", "info"]).expect("`runner info` still parses");
        Cli::try_parse_from(["runner", "info", "--json"])
            .expect("`runner info --json` still parses");

        // … but it must not advertise itself in help output.
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help.contains("\n  info"),
            "hidden `info` subcommand must not appear in --help, got:\n{help}",
        );
    }

    #[test]
    fn schema_version_rejects_out_of_range_values() {
        let err = Cli::try_parse_from(["runner", "--schema-version", "99", "info"])
            .expect_err("schema version should be bounded by clap");

        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn run_alias_parses_chain_flags_too() {
        let cli = RunAliasCli::try_parse_from(["run", "-p", "lint", "test"]).expect("parses");
        assert!(cli.mode.parallel);
        assert!(!cli.mode.sequential);
        assert_eq!(cli.task.as_deref(), Some("lint"));
        assert_eq!(cli.args, vec!["test".to_string()]);
    }

    #[test]
    fn install_accepts_task_list() {
        let cli = Cli::try_parse_from(["runner", "install", "build", "test"]).expect("parses");
        let Some(Command::Install {
            tasks,
            frozen,
            failure,
            ..
        }) = cli.command
        else {
            panic!("expected Install subcommand");
        };
        assert!(!frozen);
        assert!(!failure.keep_going);
        assert_eq!(tasks, vec!["build".to_string(), "test".to_string()]);
    }

    #[test]
    fn install_accepts_keep_going_flag() {
        let cli = Cli::try_parse_from(["runner", "install", "-k", "build"]).expect("parses");
        let Some(Command::Install { tasks, failure, .. }) = cli.command else {
            panic!("expected Install subcommand");
        };
        assert!(failure.keep_going);
        assert_eq!(tasks, vec!["build".to_string()]);
    }
}

/// Universal project task runner.
#[derive(Debug, Parser)]
#[command(
    name = "runner",
    about = clap::crate_description!(),
    help_template = "{about-with-newline}{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}",
    version,
    styles = HELP_STYLES,
    arg_required_else_help = false,
    add = SubcommandCandidates::new(task_candidates)
)]
pub(crate) struct Cli {
    /// Global options shared with [`RunAliasCli`].
    #[command(flatten)]
    pub global: GlobalOpts,

    /// Subcommand to execute. Defaults to [`Command::Info`] when absent.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags shared by both `runner` and `run`. Carried inline via
/// `#[command(flatten)]` so each binary's `--help` lists them at the
/// same level as subcommand-specific arguments — clap unrolls them as
/// if they were defined on the parent struct.
#[derive(Debug, Args)]
pub(crate) struct GlobalOpts {
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
    /// The resolver also consults `$RUNNER_PM` independently when this
    /// flag is omitted (env reads live in `crate::resolver`, not clap).
    #[arg(
        long = "pm",
        global = true,
        value_name = "NAME",
        help = PM_HELP.as_str(),
    )]
    pub pm_override: Option<String>,

    /// Override the detected task runner (e.g. `just`, `turbo`, `make`).
    /// The resolver also consults `$RUNNER_RUNNER` independently when
    /// this flag is omitted (env reads live in `crate::resolver`, not
    /// clap).
    #[arg(
        long = "runner",
        global = true,
        value_name = "NAME",
        help = RUNNER_HELP.as_str(),
    )]
    pub runner_override: Option<String>,

    /// What to do when no detection signal matches: `probe` (default,
    /// PATH probe), `npm` (legacy silent fallback), `error` (refuse).
    /// The resolver also consults `$RUNNER_FALLBACK` independently when
    /// this flag is omitted (env reads live in `crate::resolver`, not
    /// clap).
    #[arg(
        long = "fallback",
        global = true,
        value_name = "POLICY",
        help = concat!(
            "What to do when no detection signal matches: ",
            cyan!("probe"), " (default, PATH probe), ",
            cyan!("npm"), " (legacy silent fallback), ",
            cyan!("error"), " (refuse). Also reads ",
            cyan!("RUNNER_FALLBACK"), " when omitted."
        ),
    )]
    pub fallback: Option<String>,

    /// What to do when the manifest declaration (packageManager / devEngines)
    /// disagrees with the detected lockfile: `warn` (default), `error`
    /// (refuse, exit 2), `ignore` (silent). The resolver also consults
    /// `$RUNNER_ON_MISMATCH` independently when this flag is omitted.
    #[arg(
        long = "on-mismatch",
        global = true,
        value_name = "POLICY",
        help = concat!(
            "What to do when the manifest declaration disagrees with the lockfile: ",
            cyan!("warn"), " (default), ",
            cyan!("error"), " (exit 2), ",
            cyan!("ignore"), " (silent). Also reads ",
            cyan!("RUNNER_ON_MISMATCH"), " when omitted."
        ),
    )]
    pub on_mismatch: Option<String>,

    /// Print a one-line trace describing how the package manager was
    /// resolved. The resolver also enables this when `$RUNNER_EXPLAIN`
    /// is set to a truthy value (env reads live in `crate::resolver`,
    /// not clap).
    #[arg(
        long = "explain",
        global = true,
        help = concat!(
            "Print a one-line trace describing how the package manager was resolved. \
             Also enabled when ", cyan!("RUNNER_EXPLAIN"), " is set to a truthy value."
        ),
    )]
    pub explain: bool,

    /// Suppress all non-fatal warnings on stderr. Errors still surface;
    /// only `DetectionWarning` output is silenced. Also enabled when
    /// `$RUNNER_NO_WARNINGS` is set to a truthy value.
    #[arg(
        long = "no-warnings",
        global = true,
        help = concat!(
            "Suppress all non-fatal warnings on stderr. Also enabled when ",
            cyan!("RUNNER_NO_WARNINGS"), " is set to a truthy value."
        ),
    )]
    pub no_warnings: bool,

    /// Pin the JSON output schema to a specific version. Defaults to the
    /// latest version this binary produces. The chosen version controls
    /// the `source` field on tasks/decisions in `doctor`/`list`/`why`
    /// JSON output. v1 uses filename-style labels (`"justfile"`, `"bacon.toml"`),
    /// v2 uses tool names (`"just"`, `"bacon"`). The resolver, human output,
    /// and qualified-task parsing are unaffected.
    #[arg(
        long = "schema-version",
        global = true,
        value_parser = clap::value_parser!(u32).range(1..=2),
        value_name = "N",
        help = concat!(
            "Pin JSON output schema version (",
            cyan!("1"), " or ", cyan!("2"), "). Defaults to latest. Affects ",
            cyan!("--json"), " output of doctor/list/why only."
        ),
    )]
    pub schema_version: Option<u32>,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run a task, or exec a command through the detected package manager.
    /// With `-s` or `-p`, runs multiple tasks as a chain.
    #[command(alias = "r")]
    Run {
        /// Task name or command to execute. In chain mode, the first task in the chain.
        #[arg(add = ArgValueCandidates::new(task_candidates))]
        task: Option<String>,
        /// Arguments forwarded to the task, OR additional task names in
        /// chain mode. `trailing_var_arg` + `allow_hyphen_values`
        /// support the documented bare-forward shape
        /// (`runner run test --watch` → `--watch` reaches the task).
        /// Trade-off: chain-failure flags (`-k`, `--kill-on-fail`)
        /// must precede task names in chain mode
        /// (`runner run -s -k build test`), since anything after the
        /// first positional is consumed as a forwarded value.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Chain mode flags: `-s` / `-p`.
        #[command(flatten)]
        mode: ChainModeFlags,
        /// Chain failure-policy flags: `-k` / `--kill-on-fail`.
        #[command(flatten)]
        failure: ChainFailureFlags,
    },

    /// Install project dependencies, then optionally chain tasks
    /// (`runner install build test` → install → build → test, sequential).
    #[command(alias = "i")]
    Install {
        /// Reproducible install from lockfile (npm ci, --frozen-lockfile, etc.)
        #[arg(long)]
        frozen: bool,
        /// Optional task names to run after install completes. Chain is
        /// always sequential; `-p` is not accepted here. Plain positional
        /// (no `trailing_var_arg`) so chain-failure flags placed after
        /// the task list still parse as flags, not task names.
        #[arg(add = ArgValueCandidates::new(task_candidates))]
        tasks: Vec<String>,
        /// Chain failure-policy flags. `--kill-on-fail` is accepted but
        /// unused (install is always sequential).
        #[command(flatten)]
        failure: ChainFailureFlags,
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
        #[arg(long, conflicts_with_all = ["json"])]
        raw: bool,
        /// Emit JSON instead of human-readable output.
        #[arg(long, conflicts_with_all = ["raw"])]
        json: bool,
        /// Restrict output to a single source (e.g. `package.json`,
        /// `Makefile`, `justfile`).
        #[arg(long, value_name = "SOURCE")]
        source: Option<String>,
    },

    /// Deprecated alias for `list` — hidden, prints a warning, then
    /// renders the task list. Bare `runner` still shows the project
    /// dashboard; only the explicit `info` verb is deprecated.
    #[command(hide = true)]
    Info {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Diagnostic dump: every signal the resolver considers in this dir
    Doctor {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Explain how a specific task would dispatch (sources + PM trace)
    Why {
        /// Task name to analyze.
        task: String,
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

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

    /// Render roff man pages (build: --features man-gen)
    #[cfg(feature = "man-gen")]
    Man {
        /// Write every page into this dir instead of the `runner` page to stdout.
        #[arg(
            short = 'o',
            long = "output",
            value_name = "DIR",
            value_hint = clap::ValueHint::DirPath,
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
    styles = HELP_STYLES,
    arg_required_else_help = false
)]
pub(crate) struct RunAliasCli {
    /// Global options shared with [`Cli`].
    #[command(flatten)]
    pub global: GlobalOpts,

    /// Task name or command. When omitted, prints project info.
    #[arg(add = ArgValueCandidates::new(task_candidates))]
    pub task: Option<String>,

    /// Arguments forwarded to the task/command, OR additional task
    /// names in chain mode. Same `trailing_var_arg` trade-off as
    /// `Cli::Run.args`: bare forwarding supported
    /// (`run test --watch`); chain-failure flags must precede task
    /// names in chain mode.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,

    /// Chain mode flags: `-s` / `-p`.
    #[command(flatten)]
    pub mode: ChainModeFlags,

    /// Chain failure-policy flags: `-k` / `--kill-on-fail`.
    #[command(flatten)]
    pub failure: ChainFailureFlags,
}

/// Chain-mode flags shared across `Cli::Run` and `RunAliasCli`. Grouped
/// so neither subcommand exceeds clippy's `struct_excessive_bools` cap
/// of three.
#[derive(Debug, Args, Default, Clone, Copy)]
pub(crate) struct ChainModeFlags {
    /// Run the given tasks sequentially. Conflicts with `--parallel`.
    #[arg(short = 's', long, conflicts_with = "parallel")]
    pub sequential: bool,
    /// Run the given tasks in parallel. Conflicts with `--sequential`.
    #[arg(short = 'p', long)]
    pub parallel: bool,
}

/// Chain failure-policy flags shared across `Cli::Run`, `Cli::Install`,
/// and `RunAliasCli`. Mutually exclusive (`-k` vs `--kill-on-fail`)
/// enforced at the clap layer.
#[derive(Debug, Args, Default, Clone, Copy)]
pub(crate) struct ChainFailureFlags {
    /// Run every task in the chain regardless of failures. Conflicts
    /// with `--kill-on-fail`.
    #[arg(short = 'k', long, conflicts_with = "kill_on_fail")]
    pub keep_going: bool,
    /// Parallel only: SIGKILL siblings on first failure. Accepted but
    /// unused in sequential mode.
    #[arg(long, conflicts_with = "keep_going")]
    pub kill_on_fail: bool,
}
