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

/// Compact env-var suffix matching clap's `[env: VAR=]` help style.
fn env_suffix(var: &str) -> String {
    format!("[env: {}]", cyan_str(var))
}

/// Comma-joined, cyan-styled list of every [`PackageManager`] label.
/// Built once at first help-text access via [`LazyLock`]; rebuilding the
/// list on every `--help` invocation would waste work for a value that is
/// fully determined by the [`PackageManager::all`] enumeration.
static PM_HELP: LazyLock<String> = LazyLock::new(|| {
    let joined = PackageManager::all()
        .iter()
        .map(|pm| pm.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!("Force PM ({joined}) {}", env_suffix("RUNNER_PM"))
});

/// Comma-joined, cyan-styled list of every [`TaskRunner`] label.
/// Lazy-built for the same reason as [`PM_HELP`].
static RUNNER_HELP: LazyLock<String> = LazyLock::new(|| {
    let joined = TaskRunner::all()
        .iter()
        .map(|r| r.label())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Force task runner ({joined}) {}",
        env_suffix("RUNNER_RUNNER")
    )
});

/// Sort aliases after all real recipes in completion candidates by offsetting
/// their display order beyond any realistic [`TaskSource::display_order`] value.
const ALIAS_DISPLAY_ORDER_OFFSET: usize = 100;

/// Help-text ordering bands. Flattened [`GlobalOpts`] and per-command flag
/// structs register args in interleaved parse order; without explicit bands
/// `-k`/`--pm`/`-K`/`--runner` shuffle together in `--help`.
mod help_order {
    pub(super) const DIR: usize = 10;
    pub(super) const COMMAND: usize = 20;
    pub(super) const CHAIN_MODE: usize = 30;
    pub(super) const CHAIN_FAILURE: usize = 40;
    pub(super) const PM: usize = 100;
    pub(super) const RUNNER: usize = 101;
    pub(super) const FALLBACK: usize = 102;
    pub(super) const ON_MISMATCH: usize = 103;
    pub(super) const EXPLAIN: usize = 200;
    pub(super) const NO_WARNINGS: usize = 201;
    pub(super) const QUIET: usize = 202;
    pub(super) const SCHEMA_VERSION: usize = 203;
}
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
/// disambiguation via tab-completion. The bare candidate's help/label is taken
/// from the source the runtime selector would pick (default tier; see the
/// `bare_winner` computation below), not from detection order, so it names the
/// source `runner <name>` actually dispatches to.
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

    // Pick which source supplies each name's bare candidate by mirroring the
    // runtime selector's default tier (`Turbo > Package > others`, then
    // `display_order`, then recipes-before-aliases — see
    // `cmd::run::select::select_task_entry`). Previously the bare label came
    // from whichever source appeared first in detection order, which could
    // name a different source than the one `runner <name>` actually
    // dispatches to. The selector's `[task_runner].prefer` and nearest-config
    // (`source_depth`) tiebreaks need config plus a `ProjectContext` the
    // completion callback doesn't have, so the bare label aligns with the
    // *default* tier only — still strictly better than detection order, and
    // the qualified `source:name` forms remain for exact disambiguation.
    let no_overrides = crate::resolver::ResolutionOverrides::default();
    let bare_rank = |task: &crate::types::Task| {
        (
            crate::cmd::run::source_priority(&no_overrides, task.source),
            task.source.display_order(),
            task.alias_of.is_some(),
        )
    };
    let mut bare_winner: HashMap<&str, usize> = HashMap::new();
    for (idx, task) in tasks.iter().enumerate() {
        if is_self_passthrough(task) {
            continue;
        }
        let is_better = match bare_winner.get(task.name.as_str()) {
            Some(&best) => bare_rank(task) < bare_rank(&tasks[best]),
            None => true,
        };
        if is_better {
            bare_winner.insert(task.name.as_str(), idx);
        }
    }

    let mut candidates = Vec::new();
    for (idx, task) in tasks.iter().enumerate() {
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

        // The rank-winning source supplies the bare candidate (emitted once
        // per name); its label names the source `runner <name>` dispatches to.
        if bare_winner.get(task.name.as_str()) == Some(&idx) {
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
        ChainFailureFlags, Cli, Command, RunAliasCli, cli_dir_from_argv, resolve_completion_dir,
        task_candidates_from,
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
    fn bare_label_follows_dispatch_priority_not_detection_order() {
        // `package.json` is detected first, but `runner build` dispatches to
        // turbo (Turbo > Package in the default selector tier). The bare
        // candidate's label must therefore name turbo, not the detection-order
        // first source — otherwise the completion menu misreports what runs.
        let tasks = vec![
            task("build", TaskSource::PackageJson),
            task("build", TaskSource::TurboJson),
        ];
        let candidates = task_candidates_from(&tasks);
        let bare = candidates
            .iter()
            .find(|c| c.get_value().to_string_lossy() == "build")
            .expect("bare 'build' candidate must exist");
        assert_eq!(
            bare.get_tag().map(ToString::to_string).as_deref(),
            Some("turbo"),
            "bare label must name the dispatch-winning source (turbo), not the detection-order \
             first source (package.json)"
        );

        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            values.iter().filter(|v| *v == "build").count(),
            1,
            "bare 'build' should appear exactly once"
        );
        assert!(values.contains(&"package.json:build".to_string()));
        assert!(values.contains(&"turbo:build".to_string()));
    }

    #[test]
    fn bare_label_skips_suppressed_passthrough_and_ranks_real_sources() {
        // A suppressed package.json turbo-passthrough plus two real sources.
        // The passthrough must not supply the bare label, and among the real
        // sources turbo outranks make, so the bare label names turbo.
        let tasks = vec![
            turbo_passthrough("build"),
            task("build", TaskSource::Makefile),
            task("build", TaskSource::TurboJson),
        ];
        let candidates = task_candidates_from(&tasks);
        let bare = candidates
            .iter()
            .find(|c| c.get_value().to_string_lossy() == "build")
            .expect("bare 'build' candidate must exist");
        assert_eq!(
            bare.get_tag().map(ToString::to_string).as_deref(),
            Some("turbo"),
            "bare label must name the rank-winning real source, not the suppressed passthrough \
             source"
        );

        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();
        assert!(
            !values.contains(&"package.json:build".to_string()),
            "the suppressed passthrough must not surface a qualified form"
        );
        assert!(values.contains(&"make:build".to_string()));
        assert!(values.contains(&"turbo:build".to_string()));
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
        let err = Cli::try_parse_from(["runner", "run", "-s", "-k", "-K", "build", "test"])
            .expect_err("conflict");
        let msg = format!("{err}");
        assert!(msg.contains("--keep-going") || msg.contains("--kill-on-fail"));
    }

    #[test]
    fn run_parses_kill_on_fail_short_flag() {
        let cli =
            Cli::try_parse_from(["runner", "run", "-p", "-K", "build", "test"]).expect("parses");
        match cli.command {
            Some(Command::Run {
                failure:
                    ChainFailureFlags {
                        kill_on_fail: true, ..
                    },
                ..
            }) => {}
            other => panic!("expected Run with kill_on_fail=true, got {other:?}"),
        }
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
    fn install_accepts_no_scripts_flag() {
        let cli = Cli::try_parse_from(["runner", "install", "--no-scripts"]).expect("parses");
        let Some(Command::Install {
            no_scripts,
            scripts,
            ..
        }) = cli.command
        else {
            panic!("expected Install subcommand");
        };
        assert!(no_scripts, "--no-scripts should set the flag");
        assert!(!scripts, "--scripts stays off when only --no-scripts given");
    }

    #[test]
    fn install_accepts_scripts_flag() {
        let cli = Cli::try_parse_from(["runner", "install", "--scripts"]).expect("parses");
        let Some(Command::Install {
            no_scripts,
            scripts,
            ..
        }) = cli.command
        else {
            panic!("expected Install subcommand");
        };
        assert!(scripts, "--scripts should set the flag");
        assert!(
            !no_scripts,
            "--no-scripts stays off when only --scripts given"
        );
    }

    #[test]
    fn install_scripts_and_no_scripts_are_mutually_exclusive() {
        let err = Cli::try_parse_from(["runner", "install", "--scripts", "--no-scripts"])
            .expect_err("--scripts and --no-scripts must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn install_defaults_both_script_flags_to_false() {
        let cli = Cli::try_parse_from(["runner", "install"]).expect("parses");
        let Some(Command::Install {
            no_scripts,
            scripts,
            ..
        }) = cli.command
        else {
            panic!("expected Install subcommand");
        };
        assert!(!no_scripts, "--no-scripts should default off");
        assert!(!scripts, "--scripts should default off");
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

    #[test]
    fn install_accepts_parallel_flag() {
        // `-p` after the task list still parses as a flag (plain positional,
        // not trailing_var_arg) and selects parallel post-install execution.
        let cli =
            Cli::try_parse_from(["runner", "install", "build", "test", "-p"]).expect("parses");
        let Some(Command::Install { tasks, mode, .. }) = cli.command else {
            panic!("expected Install subcommand");
        };
        assert!(mode.parallel, "-p should set parallel");
        assert!(!mode.sequential);
        assert_eq!(tasks, vec!["build".to_string(), "test".to_string()]);
    }

    #[test]
    fn install_rejects_sequential_and_parallel_together() {
        let err =
            Cli::try_parse_from(["runner", "install", "-s", "-p", "build"]).expect_err("conflict");
        let msg = format!("{err}");
        assert!(msg.contains("--parallel") || msg.contains("--sequential"));
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
    /// Project directory (default: cwd).
    #[arg(
        long = "dir",
        global = true,
        env = "RUNNER_DIR",
        value_name = "PATH",
        value_hint = clap::ValueHint::DirPath,
        value_parser = clap::value_parser!(PathBuf),
        display_order = help_order::DIR,
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
        display_order = help_order::PM,
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
        display_order = help_order::RUNNER,
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
        display_order = help_order::FALLBACK,
        help = concat!(
            "No detection match: ",
            cyan!("probe"), " (default), ",
            cyan!("npm"), ", ",
            cyan!("error"), " ",
            "[env: ", cyan!("RUNNER_FALLBACK"), "]"
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
        display_order = help_order::ON_MISMATCH,
        help = concat!(
            "Manifest vs lockfile: ",
            cyan!("warn"), " (default), ",
            cyan!("error"), " (exit 2), ",
            cyan!("ignore"), " ",
            "[env: ", cyan!("RUNNER_ON_MISMATCH"), "]"
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
        display_order = help_order::EXPLAIN,
        help = concat!(
            "PM resolution trace ",
            "[env: ", cyan!("RUNNER_EXPLAIN"), "]"
        ),
    )]
    pub explain: bool,

    /// Suppress all non-fatal warnings on stderr. Errors still surface;
    /// only `DetectionWarning` output is silenced. Also enabled when
    /// `$RUNNER_NO_WARNINGS` is set to a truthy value.
    #[arg(
        long = "no-warnings",
        global = true,
        display_order = help_order::NO_WARNINGS,
        help = concat!(
            "Hide non-fatal warnings ",
            "[env: ", cyan!("RUNNER_NO_WARNINGS"), "]"
        ),
    )]
    pub no_warnings: bool,

    /// Suppress the dispatch arrow (`→ <source> <task>`) on stderr. Also
    /// silences the `--explain` trace at dispatch time. Enabled when
    /// `$RUNNER_QUIET` is set to a truthy value.
    #[arg(
        short = 'q',
        long = "quiet",
        global = true,
        display_order = help_order::QUIET,
        help = concat!(
            "Hide dispatch line + ", cyan!("--explain"), " trace ",
            "[env: ", cyan!("RUNNER_QUIET"), "]"
        ),
    )]
    pub quiet: bool,

    /// Pin the JSON output schema to a specific version. Defaults to the
    /// latest version the command produces. The chosen version controls
    /// the `source` field on tasks/decisions in `doctor`/`list`/`why`
    /// JSON output. v1 uses filename-style labels (`"justfile"`, `"bacon.toml"`),
    /// v2 uses tool names (`"just"`, `"bacon"`). v3 restructures the
    /// reports: `why` gains `{task, match}` candidate pairs plus a
    /// decision block, `doctor` becomes a structured diagnostic
    /// inventory; `list` rejects v3 until its contract lands. The
    /// resolver, human output, and qualified-task parsing are unaffected.
    #[arg(
        long = "schema-version",
        global = true,
        value_parser = clap::value_parser!(u32).range(1..=3),
        value_name = "N",
        display_order = help_order::SCHEMA_VERSION,
        help = concat!(
            "Pin ", cyan!("--json"), " schema (doctor/why ",
            cyan!("1"), "-", cyan!("3"), ", list ",
            cyan!("1"), "-", cyan!("2"), "; default latest)"
        ),
    )]
    pub schema_version: Option<u32>,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run or exec a task; `-s`/`-p` chain multiple
    #[command(
        alias = "r",
        about = concat!("Run or exec a task; ", cyan!("-s"), "/", cyan!("-p"), " chain multiple"),
    )]
    Run {
        /// Task name or command to execute. In chain mode, the first task in the chain.
        #[arg(add = ArgValueCandidates::new(task_candidates))]
        task: Option<String>,
        /// Arguments forwarded to the task, or extra task names in chain mode.
        // In chain mode, chain-failure flags (`-k`) must precede task names —
        // `trailing_var_arg` consumes everything after the first positional.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Chain mode flags: `-s` / `-p`.
        #[command(flatten)]
        mode: ChainModeFlags,
        /// Chain failure-policy flags: `-k` / `-K`.
        #[command(flatten)]
        failure: ChainFailureFlags,
    },

    /// List tasks from detected sources
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

    /// Install deps; may chain tasks after; `-s`/`-p` pick the post-install mode
    #[command(
        alias = "i",
        about = concat!("Install deps; may chain tasks after; ", cyan!("-s"), "/", cyan!("-p"), " pick the post-install mode"),
    )]
    Install {
        /// Reproducible install from lockfile (npm ci, --frozen-lockfile, etc.)
        #[arg(short = 'f', long, display_order = help_order::COMMAND)]
        frozen: bool,
        /// Skip install lifecycle scripts where the PM supports it
        /// (npm/yarn/pnpm/bun/composer; deno already denies)
        #[arg(long = "no-scripts", display_order = help_order::COMMAND + 1)]
        no_scripts: bool,
        /// Force install lifecycle scripts on where the PM can express it
        /// (npm/yarn-berry/deno; bun/pnpm need a manifest allowlist)
        #[arg(
            long = "scripts",
            conflicts_with = "no_scripts",
            display_order = help_order::COMMAND + 2
        )]
        scripts: bool,
        /// Optional task names to run after install completes. Sequential by
        /// default; `-p` runs them concurrently once install finishes (install
        /// itself always runs first, never as a parallel sibling). Plain
        /// positional (no `trailing_var_arg`) so chain flags placed after the
        /// task list still parse as flags, not task names.
        #[arg(add = ArgValueCandidates::new(task_candidates))]
        tasks: Vec<String>,
        /// Chain mode flags `-s`/`-p` — govern the post-install tasks only.
        #[command(flatten)]
        mode: ChainModeFlags,
        /// Chain failure-policy flags `-k`/`-K`. `-K` (kill siblings) only
        /// bites with `-p`; under `-s` it degrades to fail-fast.
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

    /// Deprecated alias for `list` — hidden, prints a warning, then
    /// renders the task list. Bare `runner` still shows the project
    /// dashboard; only the explicit `info` verb is deprecated.
    #[command(hide = true)]
    Info {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// How a task would dispatch
    Why {
        /// Task name to analyze.
        task: String,
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Resolver signals for this directory
    Doctor {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Manage the project `runner.toml`
    Config {
        /// Config action: `init`, `show`, `validate`, `path`.
        #[command(subcommand)]
        action: ConfigAction,
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

    /// Render roff man pages (build: --features man)
    #[cfg(feature = "man")]
    #[command(hide = true)]
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

    /// Emit JSON Schemas. Only compiled in with the `schema` cargo feature.
    #[cfg(feature = "schema")]
    #[command(about = "Emit JSON Schemas")]
    Schema {
        /// Emit every committed schema into the output directory.
        #[arg(long)]
        all: bool,
        /// Write the schema to this file, or all schemas to this directory with --all.
        #[arg(
            short = 'o',
            long = "output",
            value_name = "PATH",
            value_hint = clap::ValueHint::FilePath,
            value_parser = clap::value_parser!(PathBuf),
        )]
        output: Option<PathBuf>,
    },

    /// Run the editor language server for runner.toml over stdio.
    /// Only compiled in with the `lsp` cargo feature.
    #[cfg(feature = "lsp")]
    #[command(about = "Run the runner.toml language server (LSP) over stdio")]
    Lsp,

    /// Catch-all: treat unknown subcommands as task names.
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Actions under `runner config`. All are `Copy` so [`crate::lib`] can peek
/// the variant before the resolver setup (which `config` deliberately
/// bypasses) without moving out of the parsed [`Cli`].
#[derive(Debug, Clone, Copy, Subcommand)]
pub(crate) enum ConfigAction {
    /// Write a starter runner.toml to the project root
    Init {
        /// Overwrite an existing runner.toml instead of refusing.
        #[arg(short, long)]
        force: bool,
    },
    /// Print the effective config and where it loaded from
    Show {
        /// Emit JSON instead of TOML.
        #[arg(long)]
        json: bool,
    },
    /// Parse and validate runner.toml; exit 2 on error
    Validate,
    /// Print the resolved runner.toml path
    Path,
}

/// CLI used by the `run` alias binary. Behaves as a shortcut for
/// `runner run <task>`: the first positional is the task or command,
/// any remaining positionals are forwarded as its arguments, and
/// built-in subcommand names are never parsed specially (so
/// `run foo bar` runs `foo` with `bar`, not two separate targets).
#[derive(Debug, Parser)]
#[command(
    name = "run",
    about = "Run or exec a task via the detected package manager",
    help_template = "{about-with-newline}{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}",
    // `-h`/`--help`/`-V`/`--version` are no longer clap args (see the
    // disable note below), so document them here instead of in the options
    // list, and flag the forwarding rule that distinguishes this binary
    // from `runner run`.
    after_help = concat!(
        "\nUse ", cyan!("-h"), "/", cyan!("--help"), " or ", cyan!("-V"), "/", cyan!("--version"),
        " before a task for this binary's own help and version.\n",
        "After a task name they are forwarded to the task instead (use ", cyan!("--"), " to force forwarding).",
    ),
    styles = HELP_STYLES,
    arg_required_else_help = false,
    // clap's built-in `--help`/`--version` short-circuit parsing wherever
    // they appear, so `run <task> --help` printed *our* help instead of the
    // task's. We disable them and leave `--help`/`--version` undefined: a
    // *defined* flag would be consumed by clap even after the task (like
    // `-k`), but an *undefined* hyphen token after the first positional is
    // swallowed by `args` (`trailing_var_arg`) and forwarded to the task.
    // A leading `--help`/`--version` (before any task) instead surfaces as
    // an `UnknownArgument` error — `task` takes no hyphen values — which
    // `run_alias_in_dir` recognises as this binary's own help/version
    // request. `run <task> -- --help` keeps forwarding literally.
    disable_help_flag = true,
    disable_version_flag = true,
)]
pub(crate) struct RunAliasCli {
    /// Global options shared with [`Cli`].
    #[command(flatten)]
    pub global: GlobalOpts,

    /// Task name or command. When omitted, prints project info.
    #[arg(add = ArgValueCandidates::new(task_candidates))]
    pub task: Option<String>,

    /// Arguments forwarded to the task, or extra task names in chain mode.
    // In chain mode, chain-failure flags (`-k`) must precede task names —
    // `trailing_var_arg` consumes everything after the first positional.
    // That same rule forwards a *trailing* `--help`/`--version` to the task
    // rather than treating it as this binary's own.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,

    /// Chain mode flags: `-s` / `-p`.
    #[command(flatten)]
    pub mode: ChainModeFlags,

    /// Chain failure-policy flags: `-k` / `-K`.
    #[command(flatten)]
    pub failure: ChainFailureFlags,
}

/// Chain-mode flags shared across `Cli::Run` and `RunAliasCli`. Grouped
/// so neither subcommand exceeds clippy's `struct_excessive_bools` cap
/// of three.
#[derive(Debug, Args, Default, Clone, Copy)]
pub(crate) struct ChainModeFlags {
    /// Chain tasks in order
    #[arg(
        short = 's',
        long,
        conflicts_with = "parallel",
        display_order = help_order::CHAIN_MODE,
    )]
    pub sequential: bool,
    /// Chain tasks concurrently
    #[arg(short = 'p', long, display_order = help_order::CHAIN_MODE + 1)]
    pub parallel: bool,
}

/// Chain failure-policy flags shared across `Cli::Run`, `Cli::Install`,
/// and `RunAliasCli`. Mutually exclusive (`-k` vs `-K`) enforced at the
/// clap layer.
#[derive(Debug, Args, Default, Clone, Copy)]
pub(crate) struct ChainFailureFlags {
    /// Finish chain despite failures
    #[arg(
        short = 'k',
        long,
        conflicts_with = "kill_on_fail",
        display_order = help_order::CHAIN_FAILURE,
    )]
    pub keep_going: bool,
    /// Parallel: kill siblings on first failure
    #[arg(
        short = 'K',
        long,
        conflicts_with = "keep_going",
        display_order = help_order::CHAIN_FAILURE + 1,
    )]
    pub kill_on_fail: bool,
}
