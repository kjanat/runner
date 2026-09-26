//! Command-line interface definition via [`clap`].

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::builder::styling::{AnsiColor, Color, Style, Styles};
use clap::{Args, Parser, Subcommand};
use clap_complete::aot::Shell;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate, SubcommandCandidates};

use crate::chain::FailurePolicy;
use crate::provider::Named;
use runner_core::ProviderId;

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

/// Comma-joined, cyan-styled labels of `ids`.
fn joined(ids: Vec<ProviderId>) -> String {
    ids.into_iter()
        .map(|id| cyan_str(id.label()))
        .collect::<Vec<_>>()
        .join(", ")
}

static PM_HELP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The package manager to use ({})",
        joined(crate::provider::package_managers())
    )
});

static SOURCE_HELP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The task source that must supply the task ({})",
        joined(crate::provider::task_sources())
    )
});

static RUNTIME_HELP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The JavaScript runtime to use ({})",
        joined(crate::provider::js_runtimes())
    )
});

/// Long-form `--runtime` help: each runtime's script runner, file runner and
/// exec primitive.
static RUNTIME_LONG_HELP: LazyLock<String> = LazyLock::new(|| {
    [
        RUNTIME_HELP.as_str(),
        "",
        "Runs tasks, files and package execs on the named runtime instead of the package manager:",
        "",
        &format!(
            "  {}  node --run <task>     node <file>       npx",
            cyan_str("node")
        ),
        &format!(
            "  {}   bun --bun run <task>  bun <file>        bun x --bun",
            cyan_str("bun")
        ),
        &format!(
            "  {}  deno task <task>      deno run <file>   deno x",
            cyan_str("deno")
        ),
        "",
        "It also outranks a local file's #! line.",
    ]
    .join("\n")
});

static LIST_ONLY_HELP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "List only tasks from this source, repeatable ({})",
        joined(crate::provider::task_sources())
    )
});

/// Help for the arguments after the task token.
const ARGS_HELP: &str = concat!(
    "Arguments forwarded to the task, or extra task names in chain mode. A make target accepts \
     only ",
    cyan!("NAME=value"),
    " assignments"
);

/// Sort aliases after all real recipes in completion candidates by offsetting
/// their display order beyond any realistic [`ProviderId::display_order`] value.
const ALIAS_DISPLAY_ORDER_OFFSET: usize = 100;
/// Inside a workspace member, root tasks sort behind the member's own.
const ROOT_DISPLAY_ORDER_OFFSET: usize = 20;
/// Other members' `member:name` candidates sort behind every local task.
const MEMBER_DISPLAY_ORDER_OFFSET: usize = 40;

/// Help-text ordering bands, so flattened global and per-command flags list
/// in a stable order.
mod help_order {
    pub(super) const DIR: usize = 10;
    pub(super) const COMMAND: usize = 20;
    pub(super) const CHAIN_MODE: usize = 30;
    pub(super) const CHAIN_FAILURE: usize = 40;
    pub(super) const PM: usize = 100;
    pub(super) const RUNTIME: usize = 101;
    pub(super) const SOURCE: usize = 102;
    pub(super) const PACKAGE: usize = 103;
    pub(super) const DOWNLOAD: usize = 104;
    pub(super) const DRY_RUN: usize = 200;
    pub(super) const WARNINGS: usize = 201;
    pub(super) const QUIET: usize = 203;
    pub(super) const SCHEMA_VERSION: usize = 204;
}

/// Produce [`CompletionCandidate`]s for every detected task in the current
/// directory. Called lazily by clap's runtime completion engine, only runs
/// when the shell is actually requesting completions, never during normal
/// execution.
fn task_candidates() -> Vec<CompletionCandidate> {
    let Ok(dir) = completion_dir() else {
        return vec![];
    };
    let ctx = crate::detect::detect(&dir, &crate::resolver::ResolutionOverrides::default());
    task_candidates_from(&ctx.tasks, ctx.current_member().map(std::sync::Arc::as_ref))
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
/// Precedence (highest first), same as the resolver at runtime so
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

/// The `--long`/`-s` spellings of every root-level (flattened
/// [`GlobalOpts`]) flag that consumes a following value, read from the
/// actual clap definition. Used by [`scan_run_argv`] to
/// skip flag values while scanning for the first task word.
fn global_value_flags() -> Vec<String> {
    use clap::CommandFactory as _;
    Cli::command()
        .get_arguments()
        .filter(|arg| {
            !arg.is_positional() && !arg.is_require_equals_set() && arg.get_action().takes_values()
        })
        .flat_map(|arg| {
            arg.get_long()
                .map(|long| format!("--{long}"))
                .into_iter()
                .chain(arg.get_short().map(|short| format!("-{short}")))
        })
        .collect()
}

/// Where the task positional sits in an argv, so [`forward_args_after_task`]
/// knows how many leading bare words to walk past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskPosition {
    /// The `run` alias binary: the first bare word is the task.
    First,
    /// `runner run <task>`: the first bare word is the `run`/`r` subcommand
    /// token and the second is the task. Any other subcommand declines.
    AfterRunSubcommand,
}

/// Insert a `--` forwarding delimiter directly after the task positional,
/// so every later token reaches the task verbatim.
///
/// `args` is declared `trailing_var_arg`, but clap only starts collecting
/// raw values once that positional holds one: a flag sitting *immediately*
/// after the task is still matched against runner's own options, so
/// `run tsc -p tsconfig.json --noEmit` bound `-p` to `--parallel` and then
/// rejected `--noEmit` as a non-task positional. Inserting the delimiter
/// makes clap enforce the rule the surrounding code already documents,
/// chain flags precede task names and everything after the task belongs to
/// the task.
///
/// Returns `None` (leave argv alone) when there is nothing to forward, when
/// the user already wrote a `--`, or when the argv names a different
/// subcommand.
pub(crate) fn forward_args_after_task(
    argv: &[std::ffi::OsString],
    position: TaskPosition,
) -> Option<Vec<std::ffi::OsString>> {
    let value_flags = global_value_flags();
    let mut want_subcommand = position == TaskPosition::AfterRunSubcommand;
    let mut index = 1;

    while index < argv.len() {
        let word = argv[index].to_str()?;
        if word == "--" {
            return None;
        }
        if value_flags.iter().any(|flag| flag == word) {
            index += 2;
            continue;
        }
        if word.starts_with('-') && word != "-" {
            index += 1;
            continue;
        }
        if want_subcommand {
            if word != "run" && word != "r" {
                return None;
            }
            want_subcommand = false;
            index += 1;
            continue;
        }
        let after = index + 1;
        if argv.get(after).is_none_or(|next| next == "--") {
            return None;
        }
        let mut out = argv.to_vec();
        out.insert(after, std::ffi::OsString::from("--"));
        return Some(out);
    }
    None
}

/// Candidates for the trailing `args` positional of `run`. In chain mode
/// (`-s`/`-p` typed before the first task) the trailing words are extra
/// task names, so complete tasks; otherwise they are arguments forwarded
/// verbatim to the task, where suggesting task names would be noise,
/// so complete nothing.
fn chain_args_candidates() -> Vec<CompletionCandidate> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let scan = scan_run_argv(&argv);
    if scan.chain {
        return task_candidates();
    }
    scan.task
        .as_deref()
        .map_or_else(Vec::new, |task| task_usage_candidates(task, &scan.after))
}

/// Candidates for a task's own arguments, from the spec its source
/// declares. A source without one leaves the trailing words uncompleted.
fn task_usage_candidates(task: &str, typed: &[String]) -> Vec<CompletionCandidate> {
    let Ok(dir) = completion_dir() else {
        return vec![];
    };
    let ctx = crate::detect::detect(&dir, &crate::resolver::ResolutionOverrides::default());
    let Some(spec) = ctx
        .tasks
        .iter()
        .filter(|entry| entry.name == task && ctx.is_local(entry))
        .find_map(|entry| {
            crate::commands::run::core::usage(&ctx, entry)
                .ok()
                .flatten()
        })
    else {
        return vec![];
    };

    // A flag that takes a value swallows the next word, so there is nothing
    // of ours to offer in the position it consumes. The last entry is the
    // word being completed, which is empty at a fresh TAB, so the flag to
    // test is the one before it.
    if typed
        .iter()
        .rev()
        .take(2)
        .any(|word| spec.consumes_value_after(word))
    {
        return vec![];
    }

    let mut out: Vec<CompletionCandidate> = Vec::new();
    for flag in &spec.flags {
        for spelling in flag.spellings() {
            if typed.iter().any(|word| word == &spelling) {
                continue;
            }
            let mut candidate = CompletionCandidate::new(&spelling);
            if let Some(help) = &flag.help {
                candidate = candidate.help(Some(help.clone().into()));
            }
            out.push(candidate.tag(Some("task flags".into())));
        }
    }
    // Each positional closes its own value set, so only the one the cursor
    // sits on may contribute choices.
    if let Some(arg) = spec.args.get(positional_index(&spec, typed)) {
        for choice in &arg.choices {
            let mut candidate = CompletionCandidate::new(choice);
            if let Some(help) = &arg.help {
                candidate = candidate.help(Some(help.clone().into()));
            }
            out.push(candidate.tag(Some(arg.name.clone().into())));
        }
    }
    out
}

/// Which positional the cursor sits on: the count of already-complete
/// positional words before it, skipping flags and the values they consume.
fn positional_index(spec: &runner_core::UsageSpec, typed: &[String]) -> usize {
    // The final entry is the word being completed, not a finished one.
    let complete = typed.split_last().map_or(&[][..], |(_, rest)| rest);
    let mut index = 0;
    let mut skip_value = false;
    for word in complete {
        if skip_value {
            skip_value = false;
            continue;
        }
        if word.starts_with('-') {
            skip_value = spec.consumes_value_after(word);
            continue;
        }
        index += 1;
    }
    index
}

/// Scan the in-flight completion argv for a chain-mode flag (`-s`/`-p`,
/// long forms, or a short cluster like `-sk`) *before* the first task
/// word, mirroring dispatch, where `trailing_var_arg` means chain flags
/// must precede task names and a later `-s`/`-p` is forwarded to the
/// task instead. Same argv shape as [`cli_dir_from_argv`]: user words
/// follow the first `--`, and the word after that is the binary name.
/// Value-carrying global flags are skipped with their values so
/// `--dir /some/path -s build` still detects the chain flag.
/// What the in-flight completion argv says about the `run` being typed.
#[derive(Debug, Default, PartialEq, Eq)]
struct RunArgv {
    /// A chain flag (`-s`/`-p`) appeared before the first task word, so the
    /// trailing words are further task names.
    chain: bool,
    /// The first bare word after the globals: the task whose arguments the
    /// trailing words are.
    task: Option<String>,
    /// Words already typed after the task name.
    after: Vec<String>,
}

/// Walk the completion argv once, extracting everything the trailing-arg
/// completer needs. The argv shape and the flag-skipping rules are in the
/// match arms below.
fn scan_run_argv(argv: &[std::ffi::OsString]) -> RunArgv {
    // Flags whose value arrives as the *next* word (the `=` form needs no
    // special casing; it stays one word). Derived from the real clap
    // definition so a new value-taking global can't silently drift out of
    // sync with this scanner and get its value mistaken for the task.
    let value_flags = global_value_flags();

    // Skip past clap_complete's `--` separator, then past the binary
    // name itself (`runner` or the `run` alias).
    let start = argv.iter().position(|a| a == "--").map_or(1, |idx| idx + 1);
    let mut iter = argv.get(start + 1..).unwrap_or(&[]).iter();
    let mut subcommand_seen = false;
    while let Some(arg) = iter.next() {
        let Some(word) = arg.to_str() else {
            continue;
        };
        match word {
            "-s" | "--sequential" | "-p" | "--parallel" => {
                return RunArgv {
                    chain: true,
                    ..RunArgv::default()
                };
            }
            // The `run` subcommand token (`runner run …`); the alias
            // binary has no subcommand. Only the first bare word can be
            // it. A task literally named `run` still terminates the
            // scan below on any later occurrence.
            "run" | "r" if !subcommand_seen => subcommand_seen = true,
            _ if value_flags.iter().any(|flag| flag == word) => {
                iter.next();
            }
            // Short cluster (`-sk`, `-pK`): clap accepts combined
            // shorts, so a chain flag can hide inside one.
            _ if word.starts_with('-') && !word.starts_with("--") => {
                if word.chars().skip(1).any(|c| c == 's' || c == 'p') {
                    return RunArgv {
                        chain: true,
                        ..RunArgv::default()
                    };
                }
            }
            _ if word.starts_with('-') => {}
            // First bare word is the task; anything after it belongs to
            // the task, not the chain.
            _ => {
                return RunArgv {
                    chain: false,
                    task: Some(word.to_owned()),
                    after: iter
                        .filter_map(|rest| rest.to_str().map(str::to_owned))
                        .collect(),
                };
            }
        }
    }
    RunArgv::default()
}

/// Split tasks into other members' (`member:name` candidates) and local
/// ones: the root's and the current member's, which complete bare.
fn partition_local<'a>(
    all_tasks: &'a [crate::types::Task],
    current: Option<&crate::types::WorkspaceMember>,
) -> (Vec<&'a crate::types::Task>, Vec<&'a crate::types::Task>) {
    all_tasks.iter().partition(|task| {
        task.member
            .as_ref()
            .is_some_and(|member| current.is_none_or(|current| current.dir != member.dir))
    })
}

/// Candidates for workspace member tasks: always the `member:name` form,
/// plus the bare name when the root defines no such task and exactly one
/// member does, mirroring the root-first lookup `run` performs.
fn member_task_candidates(
    member_tasks: &[&crate::types::Task],
    root_tasks: &[&crate::types::Task],
) -> Vec<CompletionCandidate> {
    use std::collections::{HashMap, HashSet};

    let root_names: HashSet<&str> = root_tasks.iter().map(|task| task.name.as_str()).collect();
    let mut members_for_name: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut sources_for_scoped_name: HashMap<(&str, &str), HashSet<ProviderId>> = HashMap::new();
    for task in member_tasks {
        members_for_name
            .entry(task.name.as_str())
            .or_default()
            .insert(task.scope());
        sources_for_scoped_name
            .entry((task.scope(), task.name.as_str()))
            .or_default()
            .insert(task.source);
    }
    let is_self_passthrough = |task: &crate::types::Task| -> bool {
        task.passthrough_to
            .and_then(Named::as_task_source)
            .is_some_and(|peer| {
                sources_for_scoped_name
                    .get(&(task.scope(), task.name.as_str()))
                    .is_some_and(|set| set.contains(&peer))
            })
    };
    let mut candidates = Vec::new();
    let mut bare_emitted: HashSet<&str> = HashSet::new();
    for task in member_tasks {
        if is_self_passthrough(task) {
            continue;
        }
        let source_label = task.source.label();
        let help = task.description.as_ref().map_or_else(
            || source_label.to_string(),
            |desc| format!("{source_label}: {desc}"),
        );
        let tag = format!("{source_label} ({})", task.scope());
        let order =
            usize::from(task.source.provider().caps.task_priority) + MEMBER_DISPLAY_ORDER_OFFSET;
        let unique_member = members_for_name
            .get(task.name.as_str())
            .is_some_and(|members| members.len() == 1);
        if !root_names.contains(task.name.as_str())
            && unique_member
            && bare_emitted.insert(task.name.as_str())
        {
            candidates.push(
                CompletionCandidate::new(&task.name)
                    .help(Some(help.clone().into()))
                    .tag(Some(tag.clone().into()))
                    .display_order(Some(order)),
            );
        }
        candidates.push(
            CompletionCandidate::new(task.display_name().into_owned())
                .help(Some(help.into()))
                .tag(Some(tag.into()))
                .display_order(Some(order)),
        );
    }
    candidates
}

/// Index of the task supplying each name's bare candidate, in the order the
/// core ranks same-named tasks under an empty policy. A current-member task
/// outranks a same-named root task.
fn bare_winners<'a>(
    tasks: &[&'a crate::types::Task],
    swallowed: impl Fn(&crate::types::Task) -> bool,
) -> std::collections::HashMap<&'a str, usize> {
    let bare_rank = |task: &crate::types::Task| {
        let source = crate::commands::run::core::source_provider(task.source);
        (
            task.member.is_none(),
            source.map_or(u8::MAX, |id| {
                runner_providers::REGISTRY.by_id(id).caps.task_priority
            }),
            source,
            task.alias_of.is_some(),
        )
    };
    let mut bare_winner = std::collections::HashMap::new();
    for (idx, task) in tasks.iter().enumerate() {
        if swallowed(task) {
            continue;
        }
        let is_better = match bare_winner.get(task.name.as_str()) {
            Some(&best) => bare_rank(task) < bare_rank(tasks[best]),
            None => true,
        };
        if is_better {
            bare_winner.insert(task.name.as_str(), idx);
        }
    }
    bare_winner
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
/// Exception: a task whose body only forwards to another runner's
/// same-named task (`"build": "turbo run build"`) is dropped from completion
/// candidates *iff* that runner's source defines the task. Detection reads
/// the forwarding from the task body, so a real script like `"build": "vite
/// build"` keeps its qualified form. `runner list` still surfaces both
/// sources.
fn task_candidates_from(
    all_tasks: &[crate::types::Task],
    current: Option<&crate::types::WorkspaceMember>,
) -> Vec<CompletionCandidate> {
    use std::collections::{HashMap, HashSet};

    use crate::types::Task;
    use runner_core::ProviderId;

    let (member_tasks, tasks) = partition_local(all_tasks, current);
    let member_candidates = member_task_candidates(&member_tasks, &tasks);

    let mut sources_for_name: HashMap<&str, HashSet<ProviderId>> = HashMap::new();
    for task in &tasks {
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
    let is_self_passthrough = |task: &Task| -> bool {
        let Some(runner) = task.passthrough_to else {
            return false;
        };
        let Some(peer_source) = runner.as_task_source() else {
            return false;
        };
        sources_for_name
            .get(task.name.as_str())
            .is_some_and(|set| set.contains(&peer_source))
    };

    let mut effective_count: HashMap<&str, usize> = HashMap::new();
    for task in &tasks {
        if !is_self_passthrough(task) {
            *effective_count.entry(task.name.as_str()).or_default() += 1;
        }
    }

    let bare_winner = bare_winners(&tasks, is_self_passthrough);

    let mut candidates = Vec::new();
    for (idx, task) in tasks.iter().enumerate() {
        if is_self_passthrough(task) {
            continue;
        }

        let source_label = task.source.label();
        // Inside a member, root tasks sort behind the member's own.
        let scope_offset = if current.is_some() && task.member.is_none() {
            ROOT_DISPLAY_ORDER_OFFSET
        } else {
            0
        };
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
                    usize::from(task.source.provider().caps.task_priority) + scope_offset,
                )
            },
            |target| {
                let help = format!("→ {target}");
                let tag = format!("{source_label} (aliases)");
                let order = usize::from(task.source.provider().caps.task_priority)
                    + ALIAS_DISPLAY_ORDER_OFFSET;
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

        // A root task the current member shadows stays reachable as
        // `root:<name>`.
        let shadowed_by_member = task.member.is_none()
            && bare_winner
                .get(task.name.as_str())
                .is_some_and(|&best| tasks[best].member.is_some());
        if shadowed_by_member {
            candidates.push(
                CompletionCandidate::new(format!("root:{}", task.name))
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
    candidates.extend(member_candidates);
    candidates
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};

    use super::{
        Cli, Command, RunAliasCli, TaskPosition, cli_dir_from_argv, forward_args_after_task,
        resolve_completion_dir, scan_run_argv, task_candidates_from,
    };

    /// Parse the way the binary does: variables bound, exclusive groups checked.
    fn parse(words: &[&str]) -> Result<crate::invocation::Parsed<Cli>, clap::Error> {
        crate::invocation::parse(
            crate::invocation::bind(Cli::command(), crate::invocation::PREFIX),
            osv(words),
        )
    }

    fn osv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    /// Apply the delimiter insertion and render the result as plain words.
    fn forwarded(words: &[&str], position: TaskPosition) -> Option<Vec<String>> {
        forward_args_after_task(&osv(words), position).map(|argv| {
            argv.iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        })
    }

    #[test]
    fn a_flag_right_after_the_task_is_forwarded_not_reparsed() {
        // #89: `-p` belongs to tsc, but clap only starts collecting raw
        // trailing values once `args` holds one, so it bound `--parallel`
        // and then rejected `--noEmit` as a non-task positional.
        assert_eq!(
            forwarded(
                &["run", "tsc", "-p", "tsconfig.json", "--noEmit"],
                TaskPosition::First,
            )
            .expect("delimiter inserted"),
            ["run", "tsc", "--", "-p", "tsconfig.json", "--noEmit"],
        );
    }

    #[test]
    fn value_flags_before_the_task_keep_their_values() {
        assert_eq!(
            forwarded(
                &["run", "--dir", "/repo", "--quiet", "build", "-p", "3000"],
                TaskPosition::First,
            )
            .expect("delimiter inserted"),
            [
                "run", "--dir", "/repo", "--quiet", "build", "--", "-p", "3000"
            ],
        );
        assert_eq!(
            forwarded(
                &["run", "--download", "build", "-p", "3000"],
                TaskPosition::First,
            )
            .expect("delimiter inserted"),
            ["run", "--download", "build", "--", "-p", "3000"],
        );
    }

    #[test]
    fn chain_flags_before_the_task_still_reach_clap() {
        // `-s` precedes the first task, so it stays a chain flag and the
        // words after it stay task names.
        assert_eq!(
            forwarded(&["run", "-s", "build", "test"], TaskPosition::First)
                .expect("delimiter inserted"),
            ["run", "-s", "build", "--", "test"],
        );
    }

    #[test]
    fn nothing_to_forward_leaves_argv_alone() {
        assert_eq!(forwarded(&["run", "build"], TaskPosition::First), None);
        assert_eq!(forwarded(&["run"], TaskPosition::First), None);
        assert_eq!(forwarded(&["run", "--help"], TaskPosition::First), None);
    }

    #[test]
    fn an_existing_delimiter_is_never_doubled() {
        // `run tsc -- -p x` already forwards; inserting again would make the
        // task see a literal `--`, which is the delimiter-counting trap #89
        // reported.
        assert_eq!(
            forwarded(&["run", "tsc", "--", "-p", "x"], TaskPosition::First),
            None,
        );
        assert_eq!(
            forwarded(&["run", "--", "tsc", "-p", "x"], TaskPosition::First),
            None,
        );
    }

    #[test]
    fn the_runner_binary_delimits_after_the_run_subcommand() {
        assert_eq!(
            forwarded(
                &["runner", "run", "tsc", "-p", "x"],
                TaskPosition::AfterRunSubcommand,
            )
            .expect("delimiter inserted"),
            ["runner", "run", "tsc", "--", "-p", "x"],
        );
    }

    #[test]
    fn other_subcommands_are_left_untouched() {
        // `install` takes a plain positional list, and its chain flags are
        // documented to parse after the task names.
        for argv in [
            ["runner", "install", "build", "-k"],
            ["runner", "why", "build", "--json"],
            ["runner", "list", "--source", "make"],
        ] {
            assert_eq!(
                forwarded(&argv, TaskPosition::AfterRunSubcommand),
                None,
                "argv: {argv:?}",
            );
        }
    }

    #[test]
    fn an_external_task_subcommand_is_left_untouched() {
        // `runner tsc -p x` reaches clap's external-subcommand catch-all,
        // which already forwards every following word verbatim.
        assert_eq!(
            forwarded(
                &["runner", "tsc", "-p", "x"],
                TaskPosition::AfterRunSubcommand
            ),
            None,
        );
    }

    #[test]
    fn global_value_flags_reflect_clap_definition() {
        // The chain-flag scanner skips these flags' values; the list is
        // derived from clap metadata precisely so it can't drift. Pin the
        // known members so a regression in the derivation itself (e.g. a
        // filter change dropping everything) is caught.
        let flags = super::global_value_flags();
        for expected in ["--dir", "--pm", "--runtime", "--source", "--on-fail"] {
            assert!(
                flags.iter().any(|f| f == expected),
                "expected {expected} in derived value flags: {flags:?}",
            );
        }
        assert!(
            !flags.iter().any(|f| f == "--quiet"),
            "boolean flags take no value and must not be skipped-with-value: {flags:?}",
        );
        assert!(
            !flags.iter().any(|f| f == "--download"),
            "--download takes its value only after `=`: {flags:?}",
        );
    }

    #[test]
    fn version_help_is_a_styled_clap_section() {
        let mut command = Cli::command().color(clap::ColorChoice::Always);
        let help = command.render_long_help().ansi().to_string();

        assert!(help.contains("\x1b[1m\x1b[4m\x1b[33mVersion output:\x1b[0m"));
        for flag in [
            "-v",
            "-V",
            "--version",
            "--build-options",
            "--revision",
            "--json",
        ] {
            assert!(
                help.contains(&format!("\x1b[1m\x1b[36m{flag}\x1b[0m")),
                "{flag} was not rendered with clap's literal style: {help:?}",
            );
        }
    }

    /// Two positionals with distinct choices, plus a value-taking flag, so
    /// each completion position is distinguishable from the others.
    fn two_positional_spec() -> runner_core::UsageSpec {
        use runner_core::{UsageArg, UsageFlag, UsageSpec};
        UsageSpec {
            signature: "[first] [second]".to_string(),
            args: vec![
                UsageArg {
                    name: "first".to_string(),
                    help: None,
                    required: false,
                    choices: vec!["a".to_string()],
                },
                UsageArg {
                    name: "second".to_string(),
                    help: None,
                    required: false,
                    choices: vec!["b".to_string()],
                },
            ],
            flags: vec![UsageFlag {
                long: vec!["fn".to_string()],
                short: vec![],
                help: None,
                required: false,
                takes_value: true,
            }],
        }
    }

    fn typed(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn positional_index_counts_only_finished_positionals() {
        let spec = two_positional_spec();
        // Only the word being completed: still on the first positional.
        assert_eq!(super::positional_index(&spec, &typed(&[""])), 0);
        // One finished positional behind the cursor.
        assert_eq!(super::positional_index(&spec, &typed(&["a", ""])), 1);
        // A flag and the value it consumes are not positionals.
        assert_eq!(
            super::positional_index(&spec, &typed(&["--fn", "x", ""])),
            0
        );
        assert_eq!(
            super::positional_index(&spec, &typed(&["--fn", "x", "a", ""])),
            1
        );
    }

    #[test]
    fn a_value_taking_flag_before_the_cursor_suppresses_candidates() {
        // The last entry is the empty word being completed, so the flag to
        // test is the one before it. Checking only the last entry made this
        // case look fine while offering flags where the value belongs.
        let spec = two_positional_spec();
        let words = typed(&["--fn", ""]);
        assert!(
            words
                .iter()
                .rev()
                .take(2)
                .any(|word| spec.consumes_value_after(word)),
            "a pending --fn value must suppress candidates",
        );
        // Once the value is typed, the next position is open again.
        let words = typed(&["--fn", "x", ""]);
        assert!(
            !words
                .iter()
                .rev()
                .take(2)
                .any(|word| spec.consumes_value_after(word)),
        );
    }

    #[test]
    fn chain_flag_detected_before_first_task() {
        // `runner run -s build <TAB>`, chain mode, trailing words are tasks.
        assert!(
            scan_run_argv(&osv(&[
                "completer",
                "--",
                "runner",
                "run",
                "-s",
                "build",
                ""
            ]))
            .chain
        );
        // Long form + value-carrying global flag before it.
        assert!(
            scan_run_argv(&osv(&[
                "completer",
                "--",
                "runner",
                "--dir",
                "/repo",
                "run",
                "--parallel",
                "build",
                ""
            ]))
            .chain
        );
        // `run` alias binary, no subcommand token.
        assert!(scan_run_argv(&osv(&["completer", "--", "run", "-p", "build", ""])).chain);
        // Chain flag hidden in a short cluster.
        assert!(scan_run_argv(&osv(&["completer", "--", "run", "-sk", "build", ""])).chain);
    }

    #[test]
    fn chain_flag_after_first_task_is_forwarded_not_chain() {
        // `run build -p 3000 <TAB>`, `-p` lands after the task, so
        // trailing_var_arg forwards it to the task; not chain mode.
        assert!(!scan_run_argv(&osv(&["completer", "--", "run", "build", "-p", "3000", ""])).chain);
        // Plain single-task run.
        assert!(!scan_run_argv(&osv(&["completer", "--", "runner", "run", "build", ""])).chain);
        // No chain flag at all.
        assert!(!scan_run_argv(&osv(&["completer", "--", "runner", "run", ""])).chain);
    }
    use crate::types::Task;
    use runner_core::ProviderId;

    fn task(name: &str, source: ProviderId) -> Task {
        Task {
            name: name.into(),
            source,
            run_target: None,
            description: None,
            alias_of: None,
            passthrough_to: None,
            detail: crate::types::TaskDetail::default(),
            member: None,
        }
    }

    fn turbo_passthrough(name: &str) -> Task {
        Task {
            passthrough_to: Some(ProviderId::Turbo),
            detail: crate::types::TaskDetail::default(),
            ..task(name, ProviderId::PackageJson)
        }
    }

    #[test]
    fn current_member_tasks_complete_bare_and_ahead_of_root_tasks() {
        use std::sync::Arc;

        use crate::types::WorkspaceMember;

        let rfc = Arc::new(WorkspaceMember::new(
            "rfc".to_string(),
            "rfc".to_string(),
            PathBuf::from("/ws/rfc"),
        ));
        let web = Arc::new(WorkspaceMember::new(
            "web".to_string(),
            "apps/web".to_string(),
            PathBuf::from("/ws/apps/web"),
        ));
        let member = |name: &str, member: &Arc<WorkspaceMember>| Task {
            member: Some(Arc::clone(member)),
            ..task(name, ProviderId::PackageJson)
        };
        let tasks = vec![
            task("site", ProviderId::PackageJson),
            task("hello", ProviderId::PackageJson),
            member("site", &rfc),
            member("check", &rfc),
            member("site", &web),
        ];

        let candidates = task_candidates_from(&tasks, Some(&rfc));
        let values: Vec<String> = candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect();

        assert!(values.contains(&"site".to_string()));
        assert!(values.contains(&"check".to_string()));
        assert!(values.contains(&"hello".to_string()));
        assert!(
            values.contains(&"root:site".to_string()),
            "the shadowed root task stays reachable: {values:?}"
        );
        assert!(values.contains(&"web:site".to_string()));
        assert_eq!(values.iter().filter(|v| *v == "site").count(), 1);

        let order = |value: &str| {
            candidates
                .iter()
                .find(|c| c.get_value().to_string_lossy() == value)
                .and_then(clap_complete::CompletionCandidate::get_display_order)
                .expect("candidate has an order")
        };
        assert!(order("site") < order("hello"), "member first, then root");
        assert!(order("hello") < order("web:site"), "root before siblings");
    }

    #[test]
    fn qualified_candidates_emitted_for_duplicates() {
        let tasks = vec![
            task("test", ProviderId::PackageJson),
            task("test", ProviderId::Make),
            task("build", ProviderId::PackageJson),
        ];
        let candidates = task_candidates_from(&tasks, None);
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
            task("build", ProviderId::Turbo),
            task("fmt", ProviderId::PackageJson),
        ];
        let candidates = task_candidates_from(&tasks, None);
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
            task("build", ProviderId::Make),
            task("build", ProviderId::Turbo),
        ];
        let candidates = task_candidates_from(&tasks, None);
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
            "Makefile is a real definition, not a passthrough, keep its qualified form"
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
        // swallowed. The passthrough flag is set per-script-body during
        // detection, not inferred from name collisions alone.
        let tasks = vec![
            // Same name, but `passthrough_to_turbo: false` because the
            // command body is `vite build`, not `turbo run build`.
            task("build", ProviderId::PackageJson),
            task("build", ProviderId::Turbo),
        ];
        let candidates = task_candidates_from(&tasks, None);
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
        // first source; otherwise the completion menu misreports what runs.
        let tasks = vec![
            task("build", ProviderId::PackageJson),
            task("build", ProviderId::Turbo),
        ];
        let candidates = task_candidates_from(&tasks, None);
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
            task("build", ProviderId::Make),
            task("build", ProviderId::Turbo),
        ];
        let candidates = task_candidates_from(&tasks, None);
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
        let candidates = task_candidates_from(&tasks, None);

        assert!(
            candidates
                .iter()
                .any(|c| c.get_value().to_string_lossy() == "build"),
            "without a turbo.json twin, the passthrough is the only source; keep it"
        );
    }

    #[test]
    fn alias_candidate_uses_arrow_help_and_dedicated_tag() {
        let tasks = vec![
            Task {
                description: Some("Build the project".into()),
                ..task("build", ProviderId::Just)
            },
            Task {
                alias_of: Some("build".into()),
                ..task("b", ProviderId::Just)
            },
        ];
        let candidates = task_candidates_from(&tasks, None);
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
        // set in the environment, completion should reflect the CLI
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
    fn contradictory_command_line_choices_are_usage_errors() {
        for argv in [
            &["runner", "run", "-s", "-p", "build"][..],
            &["runner", "run", "-k", "-K", "-s", "build"],
            &["runner", "--on-fail", "wait", "-k", "run", "build"],
            &["runner", "install", "-s", "-p", "build"],
            &["runner", "list", "--raw", "--json"],
        ] {
            let error = parse(argv).expect_err("conflict");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn an_alias_that_repeats_the_on_fail_value_is_accepted() {
        for argv in [
            &["runner", "--on-fail", "continue", "-k", "run", "build"][..],
            &["runner", "run", "-K", "--on-fail", "kill", "build"],
        ] {
            parse(argv).unwrap_or_else(|error| panic!("{argv:?}: {error}"));
        }
    }

    #[test]
    fn failure_aliases_set_the_on_fail_setting() {
        let parsed = parse(&["runner", "-K", "run", "-p", "build", "test"]).expect("parses");
        let settings = crate::invocation::settings(
            &parsed.cli.global,
            parsed.cli.command.as_ref(),
            &parsed.origins,
        );
        assert_eq!(
            settings.on_fail,
            Some((
                crate::chain::FailurePolicy::Kill,
                crate::invocation::Origin::Cli
            ))
        );
    }

    #[test]
    fn a_negated_switch_sets_its_setting_false() {
        let parsed =
            parse(&["runner", "install", "--no-scripts", "--no-tools", "-f"]).expect("parses");
        let settings = crate::invocation::settings(
            &parsed.cli.global,
            parsed.cli.command.as_ref(),
            &parsed.origins,
        );
        let cli = crate::invocation::Origin::Cli;
        assert_eq!(settings.scripts, Some((false, cli)));
        assert_eq!(settings.tools, Some((false, cli)));
        assert_eq!(settings.frozen, Some((true, cli)));
        let later = parse(&["runner", "install", "--no-scripts", "--scripts"]).expect("parses");
        let settings = crate::invocation::settings(
            &later.cli.global,
            later.cli.command.as_ref(),
            &later.origins,
        );
        assert_eq!(settings.scripts, Some((true, cli)));
    }

    #[test]
    fn download_takes_an_optional_value() {
        let origin = crate::invocation::Origin::Cli;
        for (argv, expected) in [
            (
                &["runner", "--download", "list"][..],
                crate::config::Download::Allow,
            ),
            (
                &["runner", "--download=ask", "list"],
                crate::config::Download::Ask,
            ),
            (
                &["runner", "--no-download", "list"],
                crate::config::Download::Refuse,
            ),
            (
                &["runner", "--download", "--no-download", "list"],
                crate::config::Download::Refuse,
            ),
            (
                &["runner", "--no-download", "list", "--download=ask"],
                crate::config::Download::Ask,
            ),
        ] {
            let parsed = parse(argv).expect("parses");
            let settings = crate::invocation::settings(&parsed.cli.global, None, &parsed.origins);
            assert_eq!(settings.download, Some((expected, origin)), "{argv:?}");
        }
    }

    #[test]
    fn list_only_takes_several_sources() {
        let parsed = parse(&[
            "runner",
            "list",
            "--only",
            "just,package.json",
            "--only",
            "make",
        ])
        .expect("parses");
        let Some(Command::List { only, .. }) = parsed.cli.command else {
            panic!("expected list");
        };
        assert_eq!(
            only,
            [ProviderId::Just, ProviderId::PackageJson, ProviderId::Make]
        );
        assert!(parse(&["runner", "list", "--only", "pnpm"]).is_err());
    }

    #[test]
    fn info_subcommand_still_parses_but_is_hidden() {
        // Deprecated alias, must keep parsing (with and without --json)
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
    fn no_help_text_prints_markdown() {
        fn walk(command: &mut clap::Command, path: &str, leaks: &mut Vec<String>) {
            for help in [command.render_help(), command.render_long_help()] {
                leaks.extend(
                    help.to_string()
                        .lines()
                        .filter(|line| line.contains('`') || line.contains("**"))
                        .map(|line| format!("{path}: {line}")),
                );
            }
            for sub in command.get_subcommands_mut() {
                let path = format!("{path} {}", sub.get_name());
                walk(sub, &path, leaks);
            }
        }
        let mut command = Cli::command();
        command.build();
        let mut leaks = Vec::new();
        walk(&mut command, "runner", &mut leaks);
        assert!(leaks.is_empty(), "{}", leaks.join("\n"));
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
        let Some(Command::Install { tasks, frozen, .. }) = cli.command else {
            panic!("expected Install subcommand");
        };
        assert!(!frozen);
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
    fn install_accepts_no_tools_flag() {
        let cli = Cli::try_parse_from(["runner", "install", "--no-tools"]).expect("parses");
        let Some(Command::Install { no_tools, .. }) = cli.command else {
            panic!("expected Install subcommand");
        };
        assert!(no_tools);
        let cli = Cli::try_parse_from(["runner", "install"]).expect("parses");
        let Some(Command::Install { no_tools, .. }) = cli.command else {
            panic!("expected Install subcommand");
        };
        assert!(!no_tools);
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
}

/// Universal project task runner.
#[derive(Debug, Parser)]
#[command(
    name = "runner",
    about = clap::crate_description!(),
    help_template = "{about-with-newline}{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}",
    disable_version_flag = true,
    styles = HELP_STYLES,
    arg_required_else_help = false,
    add = SubcommandCandidates::new(task_candidates)
)]
pub(crate) struct Cli {
    /// Global options shared with [`RunAliasCli`].
    #[command(flatten)]
    pub global: GlobalOpts,

    /// Build and version output selectors.
    #[command(flatten, next_help_heading = "Version output")]
    pub version: VersionOpts,

    /// Subcommand to execute. Defaults to [`Command::Info`] when absent.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Root-level build and version output selectors shared by both binaries.
///
/// These are ordinary clap arguments so clap owns their spelling, grouping,
/// conflicts, requirements, usage, and help rendering. They deliberately are
/// not global: a selector after a task or subcommand belongs to that command.
#[derive(Debug, Args)]
#[group(skip)]
pub(crate) struct VersionOpts {
    #[command(flatten)]
    pub short: ShortVersion,

    #[command(flatten)]
    pub detailed: DetailedVersion,

    /// Print the version, build channel, revision, and dirty state.
    #[arg(long, group = "version-selector", conflicts_with = "json")]
    pub revision: bool,

    /// Emit detailed build information as JSON.
    #[arg(long, requires = "detailed-version")]
    pub json: bool,
}

/// `-v` and `-V`.
#[derive(Debug, Args)]
#[group(skip)]
pub(crate) struct ShortVersion {
    /// Print the concise version.
    #[arg(short = 'v', group = "version-selector", conflicts_with = "json")]
    pub lower: bool,

    /// Print the concise version.
    #[arg(short = 'V', group = "version-selector", conflicts_with = "json")]
    pub upper: bool,
}

impl ShortVersion {
    pub(crate) const fn requested(&self) -> bool {
        self.lower || self.upper
    }
}

/// `--version` and `--build-options`.
#[derive(Debug, Args)]
#[group(skip)]
pub(crate) struct DetailedVersion {
    /// Print detailed build information.
    #[arg(
        long = "version",
        group = "version-selector",
        group = "detailed-version"
    )]
    pub version: bool,

    /// Print detailed build information (alias for `--version`).
    #[arg(
        long = "build-options",
        group = "version-selector",
        group = "detailed-version",
        help = concat!("Print detailed build information (alias for ", cyan!("--version"), ")"),
    )]
    pub build_options: bool,
}

impl DetailedVersion {
    pub(crate) const fn requested(&self) -> bool {
        self.version || self.build_options
    }
}

/// Flags shared by both `runner` and `run`, global to every subcommand.
#[derive(Debug, Args)]
pub(crate) struct GlobalOpts {
    #[arg(
        long = "dir",
        global = true,
        value_name = "PATH",
        value_hint = clap::ValueHint::DirPath,
        value_parser = clap::value_parser!(PathBuf),
        help = "Project directory, the current one when unset",
        display_order = help_order::DIR,
    )]
    pub project_dir: Option<PathBuf>,

    #[arg(
        long = "pm",
        global = true,
        value_name = "NAME",
        value_parser = crate::provider::parse_package_manager,
        help = PM_HELP.as_str(),
        display_order = help_order::PM,
    )]
    pub pm: Option<ProviderId>,

    #[arg(
        long = "runtime",
        global = true,
        value_name = "NAME",
        value_parser = crate::provider::parse_js_runtime,
        help = RUNTIME_HELP.as_str(),
        long_help = RUNTIME_LONG_HELP.as_str(),
        display_order = help_order::RUNTIME,
    )]
    pub runtime: Option<ProviderId>,

    #[arg(
        long = "source",
        global = true,
        value_name = "SOURCE",
        value_parser = crate::provider::parse_task_source,
        help = SOURCE_HELP.as_str(),
        display_order = help_order::SOURCE,
    )]
    pub source: Option<ProviderId>,

    #[arg(
        long = "package",
        global = true,
        value_name = "NAME",
        help = "Run <TASK> as the binary npm package <NAME> declares",
        display_order = help_order::PACKAGE,
    )]
    pub package: Option<String>,

    #[command(flatten)]
    pub network: DownloadFlags,

    #[command(flatten)]
    pub failure: FailureFlags,

    #[arg(
        long = "dry-run",
        global = true,
        help = "Print what would run and why, without running it",
        display_order = help_order::DRY_RUN,
    )]
    pub dry_run: bool,

    #[command(flatten)]
    pub diagnostics: WarningFlags,

    #[arg(
        short = 'q',
        long = "quiet",
        global = true,
        action = clap::ArgAction::Count,
        display_order = help_order::QUIET,
        help = concat!("Print less, repeatable: ", cyan!("-q"), " through ", cyan!("-qqqq")),
        long_help = concat!(
            "Print less, repeatable. The variable takes the count.\n",
            "\n",
            "  ", cyan!("-q"), "     ", cyan!("1"), "  hide progress, groups, timing and the summary\n",
            "  ", cyan!("-qq"), "    ", cyan!("2"), "  also hide warnings and quiet the tool\n",
            "  ", cyan!("-qqq"), "   ", cyan!("3"), "  also hide runner's error messages\n",
            "  ", cyan!("-qqqq"), "  ", cyan!("4"), "  print nothing of runner's own"
        ),
    )]
    pub quiet: u8,

    #[arg(
        long = "schema-version",
        global = true,
        value_parser = clap::value_parser!(u32).range(1..=1),
        value_name = "N",
        display_order = help_order::SCHEMA_VERSION,
        help = concat!("Pin ", cyan!("--json"), " schema (currently always ", cyan!("1"), ")"),
    )]
    pub schema_version: Option<u32>,
}

/// `--download` and its negation.
#[derive(Debug, Args)]
pub(crate) struct DownloadFlags {
    #[arg(
        long = "download",
        global = true,
        value_name = "WHEN",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        value_parser = crate::config::Download::parse,
        help = concat!(
            "Download packages a command needs: ", cyan!("true"), ", ", cyan!("false"), " or ",
            cyan!("ask")
        ),
        display_order = help_order::DOWNLOAD,
    )]
    pub download: Option<crate::config::Download>,

    #[arg(
        long = "no-download",
        global = true,
        help = "Refuse a command that needs a download",
        display_order = help_order::DOWNLOAD + 1,
    )]
    pub no_download: bool,
}

/// `--on-fail` and its aliases.
#[derive(Debug, Args)]
pub(crate) struct FailureFlags {
    #[arg(
        long = "on-fail",
        global = true,
        value_name = "ACTION",
        value_enum,
        help = "What a chain does after a task fails",
        display_order = help_order::CHAIN_FAILURE,
    )]
    pub on_fail: Option<FailurePolicy>,

    #[arg(
        short = 'k',
        long = "keep-going",
        global = true,
        help = concat!("Alias for ", cyan!("--on-fail continue")),
        display_order = help_order::CHAIN_FAILURE + 1,
    )]
    pub keep_going: bool,

    #[arg(
        short = 'K',
        long = "kill-on-fail",
        global = true,
        help = concat!("Alias for ", cyan!("--on-fail kill")),
        display_order = help_order::CHAIN_FAILURE + 2,
    )]
    pub kill_on_fail: bool,
}

/// `--warnings` and its negation.
#[derive(Debug, Args)]
pub(crate) struct WarningFlags {
    #[arg(
        long = "warnings",
        global = true,
        help = "Print warnings",
        display_order = help_order::WARNINGS,
    )]
    pub warnings: bool,

    #[arg(
        long = "no-warnings",
        global = true,
        help = "Hide warnings",
        display_order = help_order::WARNINGS + 1,
    )]
    pub no_warnings: bool,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    #[command(
        alias = "r",
        about = concat!("Run or exec a task; ", cyan!("-s"), "/", cyan!("-p"), " chain multiple"),
    )]
    Run {
        /// Task name or command to execute. In chain mode, the first task in the chain.
        #[arg(add = ArgValueCandidates::new(task_candidates))]
        task: Option<String>,
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            add = ArgValueCandidates::new(chain_args_candidates),
            help = ARGS_HELP,
        )]
        args: Vec<String>,
        #[command(flatten)]
        mode: ChainModeFlags,
    },

    /// List tasks from detected sources
    #[command(alias = "ls")]
    List {
        /// Print bare task names, one per line
        #[arg(long)]
        raw: bool,
        /// Emit JSON instead of human-readable output
        #[arg(long)]
        json: bool,
        #[arg(
            long = "only",
            value_name = "SOURCE",
            value_delimiter = ',',
            value_parser = crate::provider::parse_task_source,
            help = LIST_ONLY_HELP.as_str(),
        )]
        only: Vec<ProviderId>,
    },

    #[command(
        alias = "i",
        about = concat!("Install deps; may chain tasks after; ", cyan!("-s"), "/", cyan!("-p"), " pick the post-install mode"),
    )]
    Install {
        /// Install exactly what the lockfile pins, without changing it
        #[arg(
            short = 'f',
            long = "frozen",
            display_order = help_order::COMMAND
        )]
        frozen: bool,
        /// Let the install update the lockfile
        #[arg(long = "no-frozen", display_order = help_order::COMMAND + 1)]
        no_frozen: bool,
        /// Run dependencies' lifecycle scripts
        #[arg(long = "scripts", display_order = help_order::COMMAND + 2)]
        scripts: bool,
        /// Skip dependencies' lifecycle scripts
        #[arg(long = "no-scripts", display_order = help_order::COMMAND + 3)]
        no_scripts: bool,
        #[arg(
            long = "tools",
            display_order = help_order::COMMAND + 4,
            help = concat!("Install detected toolchains first (", cyan!("mise install"), ")"),
        )]
        tools: bool,
        /// Skip the toolchain step
        #[arg(long = "no-tools", display_order = help_order::COMMAND + 5)]
        no_tools: bool,
        #[arg(
            add = ArgValueCandidates::new(task_candidates),
            help = concat!(
                "Tasks to run after install, in sequence; ", cyan!("-p"),
                " runs them concurrently once install finishes"
            ),
        )]
        tasks: Vec<String>,
        #[command(flatten)]
        mode: ChainModeFlags,
    },

    /// Remove caches and build artifacts
    Clean {
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        #[arg(
            long,
            help = concat!("Include framework-specific Node build dirs like ", cyan!(".next")),
        )]
        include_framework: bool,
    },

    #[command(hide = true, about = concat!("Deprecated alias for ", cyan!("list")))]
    Info {
        /// Emit JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },

    /// How a task would dispatch
    Why {
        /// Task name to analyze
        task: String,
        /// Emit JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },

    /// Resolver signals for this directory
    Doctor {
        /// Emit JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },

    #[command(about = concat!("Manage the project ", cyan!("runner.toml")))]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Generate shell completions
    Completions {
        #[arg(
            value_parser = crate::commands::parse_shell_arg,
            help = concat!(
                "Target shell, bare name (", cyan!("zsh"), ") or full path (",
                cyan!("/usr/bin/zsh"), "); defaults to ", cyan!("$SHELL")
            ),
        )]
        shell: Option<Shell>,

        /// Write the completion script to <PATH> instead of stdout
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
        /// Write every page into this dir instead of the runner page to stdout
        #[arg(
            short = 'o',
            long = "output",
            value_name = "DIR",
            value_hint = clap::ValueHint::DirPath,
            value_parser = clap::value_parser!(PathBuf),
        )]
        output: Option<PathBuf>,
    },

    #[command(about = "Emit JSON Schemas")]
    Schema {
        /// Emit every committed schema into the output directory
        #[arg(long)]
        all: bool,
        /// Write the schema to this file, or all schemas to this directory with --all
        #[arg(
            short = 'o',
            long = "output",
            value_name = "PATH",
            value_hint = clap::ValueHint::FilePath,
            value_parser = clap::value_parser!(PathBuf),
        )]
        output: Option<PathBuf>,
    },

    #[cfg(feature = "lsp")]
    #[command(about = "Run the runner.toml language server (LSP) over stdio")]
    Lsp,

    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Actions under `runner config`.
#[derive(Debug, Clone, Copy, Subcommand)]
pub(crate) enum ConfigAction {
    /// Write a starter runner.toml to the project root
    Init {
        /// Overwrite an existing runner.toml
        #[arg(short, long)]
        force: bool,
    },
    /// Print the effective config and where it loaded from
    Show {
        /// Emit JSON instead of TOML
        #[arg(long)]
        json: bool,
    },
    /// Parse and validate runner.toml; exit 2 on error
    Validate,
    /// Print the resolved runner.toml path
    Path,
}

/// CLI used by the `run` alias binary, a shortcut for `runner run <task>`.
#[derive(Debug, Parser)]
#[command(
    name = "run",
    about = "Run or exec a task via the detected package manager",
    help_template = "{about-with-newline}{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}",
    after_help = concat!(
        "\nUse a help or version flag before a task for this binary's own output.\n",
        "Combining a version selector with ", cyan!("-q"), "/", cyan!("--quiet"), " selects concise output.\n",
        "After a task name they are forwarded to the task instead (use ", cyan!("--"), " to force forwarding).",
    ),
    styles = HELP_STYLES,
    arg_required_else_help = false,
    disable_version_flag = true,
)]
pub(crate) struct RunAliasCli {
    #[command(flatten)]
    pub global: GlobalOpts,

    #[command(flatten, next_help_heading = "Version output")]
    pub version: VersionOpts,

    /// Task name or command. When omitted, prints project info.
    #[arg(add = ArgValueCandidates::new(task_candidates))]
    pub task: Option<String>,

    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        add = ArgValueCandidates::new(chain_args_candidates),
        help = ARGS_HELP,
    )]
    pub args: Vec<String>,

    #[command(flatten)]
    pub mode: ChainModeFlags,
}

/// `-s` and `-p`.
#[derive(Debug, Args, Default, Clone, Copy)]
pub(crate) struct ChainModeFlags {
    /// Chain tasks in order
    #[arg(short = 's', long, display_order = help_order::CHAIN_MODE)]
    pub sequential: bool,
    /// Chain tasks concurrently
    #[arg(short = 'p', long, display_order = help_order::CHAIN_MODE + 1)]
    pub parallel: bool,
}

impl ChainModeFlags {
    /// The chain mode the flags select, if any.
    pub(crate) const fn mode(self) -> Option<crate::chain::ChainMode> {
        if self.parallel {
            Some(crate::chain::ChainMode::Parallel)
        } else if self.sequential {
            Some(crate::chain::ChainMode::Sequential)
        } else {
            None
        }
    }
}
