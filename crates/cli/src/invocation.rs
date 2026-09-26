//! Environment variables derived from the command tree, and where each parsed
//! value came from.
//!
//! Every flag reads `RUNNER_<FLAG>` when it is global and
//! `RUNNER_<COMMAND>_<FLAG>` when a subcommand owns it. A `--no-<flag>` form
//! and a fixed-value alias share their setting's variable.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};

use clap::builder::BoolishValueParser;
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Args as _, Command};

/// The prefix every variable starts with.
pub(crate) const PREFIX: &str = "RUNNER";

/// Flags that set another flag's setting to a fixed value:
/// `(alias, setting, value)`.
pub(crate) const ALIASES: &[(&str, &str, &str)] = &[
    ("keep_going", "on_fail", "continue"),
    ("kill_on_fail", "on_fail", "kill"),
];

/// Groups of flags an invocation may give at most one of on the command line.
const EXCLUSIVE: &[&[&str]] = &[&["sequential", "parallel"], &["raw", "json"]];

/// Where a value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The command line.
    Cli,
    /// A `RUNNER_*` variable.
    Env,
}

/// The origin of every value the command line or the environment supplied,
/// keyed by argument id, the innermost subcommand's winning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Origins {
    by_id: HashMap<String, Origin>,
    last: HashMap<String, usize>,
    shadowed: HashMap<String, OsString>,
}

impl Origins {
    pub(crate) fn of(&self, id: &str) -> Option<Origin> {
        self.by_id.get(id).copied()
    }

    pub(crate) fn is_cli(&self, id: &str) -> bool {
        self.of(id) == Some(Origin::Cli)
    }

    /// `value` when the command line or environment set `id`.
    pub(crate) fn given<T>(&self, id: &str, value: T) -> Option<(T, Origin)> {
        self.of(id).map(|origin| (value, origin))
    }

    /// A `--<flag>` / `--no-<flag>` pair's setting, `None` when neither the
    /// command line nor the environment set it.
    pub(crate) fn switch(&self, id: &str, value: bool, negated: bool) -> Option<(bool, Origin)> {
        if negated && self.negated(id) {
            return Some((false, Origin::Cli));
        }
        self.given(id, value)
    }

    /// The value `id`'s variable holds when the command line also set `id`.
    pub(crate) fn shadowed(&self, id: &str) -> Option<&OsStr> {
        self.shadowed.get(id).map(OsString::as_os_str)
    }

    /// Whether `--no-<id>` is the last of the pair the command line gave.
    pub(crate) fn negated(&self, id: &str) -> bool {
        let negation = format!("no_{id}");
        self.last
            .get(&negation)
            .is_some_and(|negation| self.last.get(id).is_none_or(|positive| positive < negation))
    }

    /// Record where each long flag last appears in `args` before any `--`;
    /// clap's own indices restart at each subcommand.
    fn order(&mut self, args: &[OsString], command: &Command) {
        fn longs<'a>(command: &'a Command, out: &mut HashMap<&'a str, &'a str>) {
            for arg in command.get_arguments() {
                for long in arg.get_long_and_visible_aliases().unwrap_or_default() {
                    out.insert(long, arg.get_id().as_str());
                }
            }
            for sub in command.get_subcommands() {
                longs(sub, out);
            }
        }
        let mut ids = HashMap::new();
        longs(command, &mut ids);
        for (position, word) in args.iter().enumerate().skip(1) {
            if word == "--" {
                break;
            }
            let Some(name) = word
                .to_str()
                .and_then(|word| word.strip_prefix("--"))
                .map(|flag| flag.split_once('=').map_or(flag, |(name, _)| name))
            else {
                continue;
            };
            if let Some(id) = ids.get(name).filter(|id| self.is_cli(id)) {
                self.last.insert((*id).to_owned(), position);
            }
        }
    }

    fn collect(&mut self, matches: &ArgMatches, command: &Command) {
        for arg in command.get_arguments() {
            let id = arg.get_id().as_str();
            match matches.value_source(id) {
                Some(ValueSource::CommandLine) => {
                    self.by_id.insert(id.to_owned(), Origin::Cli);
                    if let Some(raw) = arg
                        .get_env()
                        .and_then(std::env::var_os)
                        .filter(|raw| !raw.is_empty())
                    {
                        self.shadowed.insert(id.to_owned(), raw);
                    }
                }
                Some(ValueSource::EnvVariable) => {
                    self.by_id.insert(id.to_owned(), Origin::Env);
                }
                _ => {}
            }
        }
        if let Some((name, matches)) = matches.subcommand()
            && let Some(command) = command.find_subcommand(name)
        {
            self.collect(matches, command);
        }
    }
}

/// A variable whose value its flag rejects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvIssue {
    /// The variable.
    pub var: String,
    /// Its value.
    pub raw: String,
    /// Why the flag rejects it.
    pub message: String,
    /// The subcommand path that owns the flag, empty for a global one.
    pub scope: Vec<String>,
    /// The flag's argument id.
    pub id: String,
}

/// A parsed invocation: the typed command line, the origin of each value, and
/// every variable that held a value its flag rejects.
#[derive(Debug)]
pub(crate) struct Parsed<P> {
    pub cli: P,
    pub origins: Origins,
    pub path: Vec<String>,
    pub env_issues: Vec<EnvIssue>,
}

impl<P> Parsed<P> {
    /// Issues that apply to this invocation and no command-line value
    /// overrides.
    pub(crate) fn blocking_issues(&self) -> impl Iterator<Item = &EnvIssue> {
        self.env_issues.iter().filter(|issue| {
            self.path.starts_with(&issue.scope) && !overridden_on_cli(&self.origins, &issue.id)
        })
    }
}

fn overridden_on_cli(origins: &Origins, id: &str) -> bool {
    origins.is_cli(id)
        || origins.is_cli(&format!("no_{id}"))
        || ALIASES
            .iter()
            .any(|(alias, setting, _)| *setting == id && origins.is_cli(alias))
}

/// Bind every flag in `command`'s tree to its variable. Flags the root owns
/// without being global read `root_scope`.
pub(crate) fn bind(command: Command, root_scope: &str) -> Command {
    let skip: Vec<String> = crate::args::VersionOpts::augment_args(Command::new("version"))
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect();
    bind_in(command, root_scope, &skip)
}

fn bind_in(mut command: Command, scope: &str, skip: &[String]) -> Command {
    let bound: Vec<(String, String)> = command
        .get_arguments()
        .filter(|arg| !skip.iter().any(|id| id == arg.get_id().as_str()))
        .filter(|arg| bindable(&command, arg))
        .filter_map(|arg| {
            let long = arg.get_long()?;
            let scope = if arg.is_global_set() { PREFIX } else { scope };
            Some((arg.get_id().to_string(), variable(scope, long)))
        })
        .collect();
    for (id, name) in bound {
        command = command.mut_arg(id, |arg| {
            let arg = arg.env(name).hide_env_values(true);
            if matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
                arg.value_parser(BoolishValueParser::new())
            } else {
                arg
            }
        });
    }
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in names {
        let scope = variable(scope, &name);
        command = command.mut_subcommand(name, |sub| bind_in(sub, &scope, &[]));
    }
    command
}

fn variable(scope: &str, name: &str) -> String {
    format!("{scope}_{}", name.replace('-', "_").to_uppercase())
}

fn bindable(command: &Command, arg: &Arg) -> bool {
    if arg.is_positional()
        || matches!(
            arg.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
        || ALIASES
            .iter()
            .any(|(alias, ..)| *alias == arg.get_id().as_str())
    {
        return false;
    }
    let Some(long) = arg.get_long() else {
        return false;
    };
    !long.strip_prefix("no-").is_some_and(|positive| {
        command
            .get_arguments()
            .any(|other| other.get_long() == Some(positive))
    })
}

/// Detach every variable whose value its flag rejects, so the parse can go on,
/// and report each one.
pub(crate) fn detach_invalid(command: Command) -> (Command, Vec<EnvIssue>) {
    let mut issues = Vec::new();
    let command = detach_in(command, &[], &mut issues);
    (command, issues)
}

fn detach_in(mut command: Command, scope: &[String], issues: &mut Vec<EnvIssue>) -> Command {
    let mut detach = Vec::new();
    for arg in command.get_arguments() {
        let Some(var) = arg.get_env() else {
            continue;
        };
        let Some(raw) = std::env::var_os(var) else {
            continue;
        };
        if raw.is_empty() {
            detach.push(arg.get_id().to_string());
            continue;
        }
        if let Err(message) = check(arg, &raw) {
            detach.push(arg.get_id().to_string());
            if arg.is_global_set() && issues.iter().any(|issue| *issue.var == *var) {
                continue;
            }
            issues.push(EnvIssue {
                var: var.to_string_lossy().into_owned(),
                raw: raw.to_string_lossy().into_owned(),
                message,
                scope: if arg.is_global_set() {
                    Vec::new()
                } else {
                    scope.to_vec()
                },
                id: arg.get_id().to_string(),
            });
        }
    }
    if !detach.is_empty() {
        command = command.mut_args(|arg| {
            if detach.iter().any(|id| id == arg.get_id().as_str()) {
                arg.env(None)
            } else {
                arg
            }
        });
    }
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in names {
        let mut path = scope.to_vec();
        path.push(name.clone());
        command = command.mut_subcommand(name, |sub| detach_in(sub, &path, issues));
    }
    command
}

/// Run `raw` through `arg`'s own value parser.
fn check(arg: &Arg, raw: &OsStr) -> Result<(), String> {
    let mut probe = Arg::new("value")
        .long("value")
        .action(ArgAction::Set)
        .allow_hyphen_values(true)
        .value_parser(arg.get_value_parser().clone());
    if let Some(delimiter) = arg.get_value_delimiter() {
        probe = probe.value_delimiter(delimiter).action(ArgAction::Append);
    }
    let mut word = OsString::from("--value=");
    word.push(raw);
    Command::new("check")
        .no_binary_name(true)
        .arg(probe)
        .try_get_matches_from([word])
        .map(drop)
        .map_err(|error| {
            use std::error::Error as _;
            let possible: Vec<String> = arg
                .get_possible_values()
                .iter()
                .map(|value| value.get_name().to_owned())
                .collect();
            error.source().map_or_else(
                || {
                    if possible.is_empty() {
                        error.kind().to_string()
                    } else {
                        format!("expected one of {}", possible.join(", "))
                    }
                },
                ToString::to_string,
            )
        })
}

/// Parse `args` against `command`: bind variables, detach invalid ones, parse,
/// and refuse contradictory command-line choices.
///
/// # Errors
/// clap's own parse errors, and a conflict error for two command-line flags of
/// one exclusive group or two that give one setting different values.
pub(crate) fn parse<P: clap::FromArgMatches>(
    command: Command,
    args: Vec<OsString>,
) -> Result<Parsed<P>, clap::Error> {
    let (mut command, env_issues) = detach_invalid(command);
    let mut matches = command.try_get_matches_from_mut(args.clone())?;
    let mut origins = Origins::default();
    origins.collect(&matches, &command);
    let mut displaced = Vec::new();
    for group in EXCLUSIVE {
        let given: Vec<&&str> = group.iter().filter(|id| origins.is_cli(id)).collect();
        if let [first, second, ..] = given.as_slice() {
            return Err(command.error(
                clap::error::ErrorKind::ArgumentConflict,
                format!(
                    "{} cannot be used with {}",
                    flag(&command, first),
                    flag(&command, second)
                ),
            ));
        }
        let from_env: Vec<&&str> = group
            .iter()
            .filter(|id| origins.of(id) == Some(Origin::Env))
            .collect();
        if given.is_empty() {
            let set: Vec<&&&str> = from_env.iter().filter(|id| is_set(&matches, id)).collect();
            if let [first, second, ..] = set.as_slice() {
                return Err(command.error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!(
                        "{} cannot be used with {}",
                        env_name(&command, first),
                        env_name(&command, second)
                    ),
                ));
            }
        } else {
            displaced.extend(from_env.into_iter().map(|id| (*id).to_owned()));
        }
    }
    if !displaced.is_empty() {
        command = detach_ids(command, &displaced);
        matches = command.try_get_matches_from_mut(args.clone())?;
        origins = Origins::default();
        origins.collect(&matches, &command);
    }
    if let Some((first, second)) = contradiction(&matches, &origins) {
        return Err(command.error(
            clap::error::ErrorKind::ArgumentConflict,
            format!(
                "{} cannot be used with {}",
                flag(&command, first),
                flag(&command, second)
            ),
        ));
    }
    origins.order(&args, &command);
    let mut path = Vec::new();
    let mut at = &matches;
    while let Some((name, sub)) = at.subcommand() {
        path.push(name.to_owned());
        at = sub;
    }
    let cli = P::from_arg_matches(&matches)?;
    Ok(Parsed {
        cli,
        origins,
        path,
        env_issues,
    })
}

/// Two command-line flags that give one aliased setting different values.
fn contradiction(matches: &ArgMatches, origins: &Origins) -> Option<(&'static str, &'static str)> {
    let mut given: Vec<(&'static str, &'static str, String)> = ALIASES
        .iter()
        .filter(|(alias, ..)| origins.is_cli(alias))
        .map(|(alias, setting, value)| (*alias, *setting, (*value).to_owned()))
        .collect();
    for (_, setting, _) in ALIASES {
        if origins.is_cli(setting)
            && let Some(value) = cli_value(matches, setting)
            && !given.iter().any(|(id, ..)| id == setting)
        {
            given.push((setting, setting, value));
        }
    }
    given.iter().find_map(|(first, setting, value)| {
        given
            .iter()
            .find(|(_, other, other_value)| other == setting && other_value != value)
            .map(|(second, ..)| (*first, *second))
    })
}

/// The last command-line value of `id` at the innermost level that has one.
fn cli_value(matches: &ArgMatches, id: &str) -> Option<String> {
    let inner = matches.subcommand().and_then(|(_, sub)| cli_value(sub, id));
    inner.or_else(|| {
        (matches.value_source(id) == Some(ValueSource::CommandLine))
            .then(|| matches.get_raw(id)?.next_back())
            .flatten()
            .map(|raw| raw.to_string_lossy().into_owned())
    })
}

/// `command` with the variables of the arguments `ids` detached, throughout
/// its tree.
fn detach_ids(command: Command, ids: &[String]) -> Command {
    let mut command = command.mut_args(|arg| {
        if ids.iter().any(|id| id == arg.get_id().as_str()) {
            arg.env(None)
        } else {
            arg
        }
    });
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in names {
        command = command.mut_subcommand(name, |sub| detach_ids(sub, ids));
    }
    command
}

/// Whether the switch `id` is on at any level of `matches`.
fn is_set(matches: &ArgMatches, id: &str) -> bool {
    matches.try_get_one::<bool>(id).ok().flatten() == Some(&true)
        || matches.subcommand().is_some_and(|(_, sub)| is_set(sub, id))
}

/// The variable the argument `id` reads, anywhere in `command`'s tree.
fn env_name(command: &Command, id: &str) -> String {
    fn find<'a>(command: &'a Command, id: &str) -> Option<&'a Arg> {
        command
            .get_arguments()
            .find(|arg| arg.get_id().as_str() == id && arg.get_env().is_some())
            .or_else(|| command.get_subcommands().find_map(|sub| find(sub, id)))
    }
    find(command, id)
        .and_then(Arg::get_env)
        .map_or_else(|| id.to_owned(), |var| var.to_string_lossy().into_owned())
}

/// `--long` for the argument `id` anywhere in `command`'s tree.
fn flag(command: &Command, id: &str) -> String {
    fn find<'a>(command: &'a Command, id: &str) -> Option<&'a Arg> {
        command
            .get_arguments()
            .find(|arg| arg.get_id().as_str() == id)
            .or_else(|| command.get_subcommands().find_map(|sub| find(sub, id)))
    }
    find(command, id)
        .and_then(Arg::get_long)
        .map_or_else(|| id.to_owned(), |long| format!("--{long}"))
}

/// The resolver's view of a parsed command line.
pub(crate) fn settings(
    global: &crate::args::GlobalOpts,
    command: Option<&crate::args::Command>,
    origins: &Origins,
) -> crate::resolver::Invocation {
    use crate::args::Command as C;
    let on_fail = if global.failure.kill_on_fail {
        Some((crate::chain::FailurePolicy::Kill, Origin::Cli))
    } else if global.failure.keep_going {
        Some((crate::chain::FailurePolicy::Continue, Origin::Cli))
    } else {
        global
            .failure
            .on_fail
            .and_then(|policy| origins.given("on_fail", policy))
    };
    let download = if origins.negated("download") {
        Some((crate::config::Download::Refuse, Origin::Cli))
    } else {
        global
            .network
            .download
            .and_then(|value| origins.given("download", value))
    };
    let install = |id: &str, value: bool, negated: bool| match command {
        Some(C::Install { .. }) => origins.switch(id, value, negated),
        _ => None,
    };
    let (frozen, scripts, tools) = match command {
        Some(C::Install {
            frozen,
            no_frozen,
            scripts,
            no_scripts,
            tools,
            no_tools,
            ..
        }) => (
            install("frozen", *frozen, *no_frozen),
            install("scripts", *scripts, *no_scripts),
            install("tools", *tools, *no_tools),
        ),
        _ => (None, None, None),
    };
    crate::resolver::Invocation {
        pm: global.pm.and_then(|pm| origins.given("pm", pm)),
        runtime: global
            .runtime
            .and_then(|runtime| origins.given("runtime", runtime)),
        source: global
            .source
            .and_then(|source| origins.given("source", source)),
        package: global.package.clone(),
        download,
        on_fail,
        dry_run: global.dry_run,
        quiet: origins
            .given("quiet", global.quiet)
            .into_iter()
            .chain(
                origins
                    .shadowed("quiet")
                    .and_then(OsStr::to_str)
                    .and_then(|raw| raw.parse().ok())
                    .map(|count| (count, Origin::Env)),
            )
            .collect(),
        warnings: origins.switch(
            "warnings",
            global.diagnostics.warnings,
            global.diagnostics.no_warnings,
        ),
        frozen,
        scripts,
        tools,
        group_active: std::env::var(crate::commands::GROUP_ACTIVE_ENV)
            .ok()
            .and_then(|raw| crate::config::boolean(&raw))
            .unwrap_or(false),
    }
}

/// Maximum characters of a variable's value an error repeats.
const MAX_RAW_DISPLAY: usize = 60;

/// `raw` with control characters escaped and cut to [`MAX_RAW_DISPLAY`].
pub(crate) fn sanitize(raw: &str) -> String {
    let escaped: String = raw.chars().flat_map(char::escape_debug).collect();
    let mut chars = escaped.chars();
    let truncated: String = chars.by_ref().take(MAX_RAW_DISPLAY).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// `message` with every copy of `raw` in it sanitized.
pub(crate) fn sanitize_message(raw: &str, message: &str) -> String {
    let sanitized = sanitize(raw);
    let escaped: String = raw.chars().flat_map(char::escape_debug).collect();
    message
        .replace(raw, &sanitized)
        .replace(&escaped, &sanitized)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::{PREFIX, bind};

    fn variables(command: &clap::Command, path: &str, out: &mut Vec<String>) {
        for arg in command.get_arguments() {
            if let Some(var) = arg.get_env() {
                out.push(format!("{path}{}={}", arg.get_id(), var.to_string_lossy()));
            }
        }
        for sub in command.get_subcommands() {
            variables(sub, &format!("{path}{} ", sub.get_name()), out);
        }
    }

    #[test]
    fn variables_follow_the_flag_and_the_command_that_owns_it() {
        let command = bind(crate::args::Cli::command(), PREFIX);
        let mut out = Vec::new();
        variables(&command, "", &mut out);
        for expected in [
            "project_dir=RUNNER_DIR",
            "pm=RUNNER_PM",
            "runtime=RUNNER_RUNTIME",
            "source=RUNNER_SOURCE",
            "download=RUNNER_DOWNLOAD",
            "on_fail=RUNNER_ON_FAIL",
            "package=RUNNER_PACKAGE",
            "dry_run=RUNNER_DRY_RUN",
            "quiet=RUNNER_QUIET",
            "warnings=RUNNER_WARNINGS",
            "schema_version=RUNNER_SCHEMA_VERSION",
            "install frozen=RUNNER_INSTALL_FROZEN",
            "install scripts=RUNNER_INSTALL_SCRIPTS",
            "install tools=RUNNER_INSTALL_TOOLS",
            "install sequential=RUNNER_INSTALL_SEQUENTIAL",
            "run parallel=RUNNER_RUN_PARALLEL",
            "list only=RUNNER_LIST_ONLY",
            "list json=RUNNER_LIST_JSON",
            "doctor json=RUNNER_DOCTOR_JSON",
            "config init force=RUNNER_CONFIG_INIT_FORCE",
        ] {
            assert!(
                out.iter().any(|line| line == expected),
                "{expected}: {out:#?}"
            );
        }
        for unbound in [
            "no_warnings",
            "no_download",
            "keep_going",
            "kill_on_fail",
            "install no_scripts",
            "lower",
            "revision",
        ] {
            assert!(
                !out.iter()
                    .any(|line| line.starts_with(&format!("{unbound}="))),
                "{unbound}: {out:#?}"
            );
        }
    }

    #[test]
    fn the_alias_binary_scopes_its_own_flags_as_run() {
        let command = bind(crate::args::RunAliasCli::command(), "RUNNER_RUN");
        let mut out = Vec::new();
        variables(&command, "", &mut out);
        assert!(
            out.contains(&"sequential=RUNNER_RUN_SEQUENTIAL".to_owned()),
            "{out:#?}"
        );
        assert!(out.contains(&"pm=RUNNER_PM".to_owned()), "{out:#?}");
    }
}
