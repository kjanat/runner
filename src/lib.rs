//! # Runner (`runner-run` crate)
//!
//! ## Overview
//!
//! Universal project task runner.
//!
//! `runner` auto-detects your project's toolchain (package managers, task
//! runners, version constraints) and provides a unified interface to run
//! tasks, install dependencies, clean artifacts, and execute ad-hoc commands.
//!
//! ## Supported Ecosystems
//!
//! **Package managers/ecosystems:** [npm], [yarn], [pnpm], [bun], [cargo],
//! [deno], [uv], [poetry], [pipenv], [go], [bundler], [composer]
//!
//! **Task runners:** [turbo], [nx], [make], [just], [go-task], [mise], [bacon]
//!
//! [npm]: https://www.npmjs.com/
//! [yarn]: https://yarnpkg.com/
//! [pnpm]: https://pnpm.io/
//! [bun]: https://bun.sh/
//! [cargo]: https://doc.rust-lang.org/cargo/
//! [deno]: https://deno.land/
//! [uv]: https://github.com/astral-sh/uv/
//! [poetry]: https://python-poetry.org/
//! [pipenv]: https://pipenv.pypa.io/
//! [go]: https://go.dev/
//! [bundler]: https://bundler.io/
//! [composer]: https://getcomposer.org/
//! [turbo]: https://turborepo.dev/
//! [nx]: https://nx.dev/
//! [make]: https://www.gnu.org/software/make/
//! [just]: https://just.systems/
//! [go-task]: https://taskfile.dev/
//! [mise]: https://mise.jdx.dev/
//! [bacon]: https://dystroy.org/bacon/
//!
//! ## Library API
//!
//! - [`run_from_env`] parses process args and dispatches in current dir.
//! - [`run_from_args`] parses explicit args and dispatches in current dir.
//! - [`run_in_dir`] parses explicit args and dispatches against a given dir.
//!
//! ## CLI Usage
//!
//! ```bash
//! runner              # show detected project info
//! runner <task>       # run a task (falls back to package-manager exec)
//! run <task>          # alias binary: a same-named task wins, else the
//!                     #   built-in default (install/clean/list/info/
//!                     #   completions), else PM exec
//! runner run <target> # explicit unified run: task → built-in → PM exec
//! runner install      # ALWAYS the built-in (deps); a task named `install`
//!                     #   is reached via `run install`
//! runner clean        # remove caches and build artifacts (always built-in)
//! runner list         # list available tasks from all sources (always built-in)
//! ```
// Generate docs with `cargo doc --document-private-items --open`.

#![doc(
    html_logo_url = "https://raw.githubusercontent.com/kjanat/runner/d876a0b9716806d92e07f5d5560b022b6158ecd5/branding/icon.svg",
    html_favicon_url = "https://raw.githubusercontent.com/kjanat/runner/d876a0b9716806d92e07f5d5560b022b6158ecd5/branding/icon.svg"
)]

pub(crate) mod chain;
mod cli;
mod cmd;
mod complete;
mod config;
mod detect;
mod resolver;
mod schema;
mod tool;
mod types;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{CommandFactory, FromArgMatches};
use colored::Colorize;

use resolver::ResolveError;

/// JSON Schema for `runner.toml`. Built under the `schema` feature;
/// `runner schema` renders it.
#[cfg(feature = "schema")]
#[must_use]
pub fn config_schema() -> schemars::Schema {
    schemars::schema_for!(config::RunnerConfig)
}

/// Exit code semantics:
/// - `0` — success
/// - `1` — generic failure (I/O, detection, child-process non-zero)
/// - `2` — resolver could not satisfy intent (typed resolver error)
///
/// `main` and `bin/run.rs` use this to map an [`anyhow::Error`] to the
/// right code: anything that downcasts to the internal resolver-error
/// type is 2, everything else is 1. The resolver-error type itself is
/// crate-private; only the exit-code projection is part of the
/// library's public surface.
#[must_use]
pub fn exit_code_for_error(err: &anyhow::Error) -> i32 {
    if err.downcast_ref::<ResolveError>().is_some() {
        2
    } else {
        1
    }
}

const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = clap::crate_version!();

/// Parse process args, detect current dir, dispatch, return exit code.
///
/// When the `COMPLETE` environment variable is set (e.g. `COMPLETE=zsh`),
/// this function writes shell completions to stdout and exits without
/// running the normal command dispatch.
///
/// # Errors
///
/// Returns an error when reading current dir fails, project detection fails,
/// command execution fails, or writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_from_env() -> Result<i32> {
    let bin = bin_name_from_arg0(&std::env::args_os().next().unwrap_or_default())
        .unwrap_or_else(|| "runner".to_string());
    clap_complete::CompleteEnv::with_factory(move || {
        configure_cli_command(cli::Cli::command(), true)
            .name(bin.clone())
            .bin_name(bin.clone())
    })
    .shells(complete::SHELLS)
    .complete();
    run_from_args(std::env::args_os())
}

/// Parse explicit args, detect current dir, dispatch, return exit code.
///
/// `args` must include `argv[0]` as first item.
///
/// # Errors
///
/// Returns an error when reading current dir fails, project detection fails,
/// command execution fails, or writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_from_args<I, T>(args: I) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cwd = std::env::current_dir()?;
    run_in_dir(args, &cwd)
}

/// Parse explicit args and run against `dir`.
///
/// `args` must include `argv[0]` as first item.
///
/// # Errors
///
/// Returns an error when project detection fails, command execution fails, or
/// writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_in_dir<I, T>(args: I, dir: &Path) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();

    if requests_version(&args) {
        println!("{}", version_line(&args, std::io::stdout().is_terminal()));
        return Ok(0);
    }

    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(err) => return render_clap_error(&err),
    };
    let project_dir = resolve_project_dir(
        configured_project_dir(
            cli.global.project_dir.as_deref(),
            std::env::var_os("RUNNER_DIR").as_deref(),
        )
        .as_deref(),
        dir,
    )?;
    dispatch(cli, &project_dir)
}

fn parse_cli<I, T>(args: I) -> Result<cli::Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();

    let mut command = configure_cli_command(cli::Cli::command(), std::io::stdout().is_terminal());
    if let Some(bin_name) = args.first().and_then(bin_name_from_arg0) {
        command = command.name(bin_name.clone()).bin_name(bin_name);
    }
    command = shorten_help_subcommand(command);

    let matches = command.try_get_matches_from(args)?;
    cli::Cli::from_arg_matches(&matches)
}

/// Replace clap's verbose default `help` subcommand description
/// (`"Print this message or the help of the given subcommand(s)"`) with a terse
/// one. clap only injects the implicit `help` subcommand during `Command::build`,
/// so force the build first; the `Built` flag makes the later parse-time build a
/// no-op. Guarded with `find_subcommand` because a flat command without
/// subcommands (the `run` alias) never gets a `help` entry, and `mut_subcommand`
/// panics on a missing name. Must run after `name`/`bin_name` are set, since
/// `build` snapshots bin names.
fn shorten_help_subcommand(mut command: clap::Command) -> clap::Command {
    command.build();
    if command.find_subcommand("help").is_some() {
        command.mut_subcommand("help", |help| help.about("Print help for a subcommand"))
    } else {
        command
    }
}

/// Parse process args as the `run` alias binary, detect the current dir,
/// dispatch, and return the exit code.
///
/// Always treats positional arguments as a task or command (routed through
/// `cmd::run`) — built-in subcommand names are never parsed specially, so
/// `run clean`, `run install`, etc. run a same-named project task when one
/// exists. When no such task exists, a bare run token naming a built-in verb
/// (`install`/`clean`/`list`/`info`/`completions`) falls back to that
/// built-in's default form rather than the package-manager exec path.
///
/// When the `COMPLETE` environment variable is set, writes shell completions
/// to stdout and exits without running the normal command dispatch.
///
/// # Errors
///
/// Returns an error when reading current dir fails, project detection fails,
/// command execution fails, or writing clap output fails.
///
/// Argument parsing/help/version flows are rendered by clap and returned as an
/// exit code instead of terminating the host process.
pub fn run_alias_from_env() -> Result<i32> {
    let bin = bin_name_from_arg0(&std::env::args_os().next().unwrap_or_default())
        .unwrap_or_else(|| "run".to_string());
    clap_complete::CompleteEnv::with_factory(move || {
        configure_cli_command(cli::RunAliasCli::command(), true)
            .name(bin.clone())
            .bin_name(bin.clone())
    })
    .shells(complete::SHELLS)
    .complete();
    run_alias_from_args(std::env::args_os())
}

/// Parse explicit args as the `run` alias binary, detect current dir,
/// dispatch, and return the exit code. See [`run_alias_from_env`].
///
/// `args` must include `argv[0]` as first item.
///
/// # Errors
///
/// Returns an error when reading current dir fails, project detection fails,
/// command execution fails, or writing clap output fails.
pub fn run_alias_from_args<I, T>(args: I) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cwd = std::env::current_dir()?;
    run_alias_in_dir(args, &cwd)
}

/// Parse explicit args as the `run` alias binary against `dir`.\
/// See [`run_alias_from_env`].
///
/// `args` must include `argv[0]` as first item.
///
/// # Errors
///
/// Returns an error when project detection fails, command execution fails, or
/// writing clap output fails.
pub fn run_alias_in_dir<I, T>(args: I, dir: &Path) -> Result<i32>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();

    if requests_version(&args) {
        println!("{}", version_line(&args, std::io::stdout().is_terminal()));
        return Ok(0);
    }

    let cli = match parse_run_alias_cli(args.clone()) {
        Ok(cli) => cli,
        // A `--help`/`--version` *before* any task is this binary's own:
        // clap's built-ins are disabled and the flag is undefined, so it
        // can't fill the hyphen-rejecting `task` positional and surfaces as
        // `UnknownArgument`. (A *trailing* one is swallowed by `args` and
        // forwarded instead — see `cli::RunAliasCli`.) Covers the bare
        // `run --help` as well as `run --pm npm --help`, `run --dir … -V`.
        Err(err) => {
            return match alias_builtin_request(&err) {
                Some(AliasBuiltin::Help) => print_run_alias_help(&args),
                Some(AliasBuiltin::Version) => {
                    println!("{}", version_line(&args, std::io::stdout().is_terminal()));
                    Ok(0)
                }
                None => render_clap_error(&err),
            };
        }
    };

    let project_dir = resolve_project_dir(
        configured_project_dir(
            cli.global.project_dir.as_deref(),
            std::env::var_os("RUNNER_DIR").as_deref(),
        )
        .as_deref(),
        dir,
    )?;
    dispatch_run_alias(cli, &project_dir)
}

/// This binary's own help/version, requested *before* any task.
enum AliasBuiltin {
    Help,
    Version,
}

/// Classify a `run`-alias parse failure as a request for this binary's own
/// help/version, or `None` for an unrelated error to surface verbatim.
///
/// With clap's built-in `--help`/`--version` disabled and undefined, a
/// leading `-h`/`--help`/`-V`/`--version` cannot fill the hyphen-rejecting
/// `task` positional, so clap reports [`ErrorKind::UnknownArgument`] naming
/// the offending flag. A *trailing* one never reaches here — it is captured
/// by `args` and forwarded — so an `UnknownArgument` naming a help/version
/// flag unambiguously means "before any task", i.e. ours to handle.
fn alias_builtin_request(err: &clap::Error) -> Option<AliasBuiltin> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    if err.kind() != ErrorKind::UnknownArgument {
        return None;
    }
    match err.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(arg)) => match arg.as_str() {
            "--help" | "-h" => Some(AliasBuiltin::Help),
            "--version" | "-V" => Some(AliasBuiltin::Version),
            _ => None,
        },
        _ => None,
    }
}

/// Render the `run` alias binary's own help to stdout, returning exit 0.
///
/// Invoked when `-h`/`--help` precedes any task. A help flag that *follows*
/// a task is forwarded to that task instead (see [`cli::RunAliasCli`]), so
/// this path is only reached for `run`'s own help. The bin name is taken
/// from `argv[0]` so the `Usage:` line reads `run`, matching how clap's
/// built-in help rendered before it was disabled.
fn print_run_alias_help(args: &[OsString]) -> Result<i32> {
    let mut command =
        configure_cli_command(cli::RunAliasCli::command(), std::io::stdout().is_terminal());
    if let Some(bin_name) = args.first().and_then(bin_name_from_arg0) {
        command = command.name(bin_name.clone()).bin_name(bin_name);
    }
    command.print_help()?;
    Ok(0)
}

fn parse_run_alias_cli<I, T>(args: I) -> Result<cli::RunAliasCli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();

    let mut command =
        configure_cli_command(cli::RunAliasCli::command(), std::io::stdout().is_terminal());
    if let Some(bin_name) = args.first().and_then(bin_name_from_arg0) {
        command = command.name(bin_name.clone()).bin_name(bin_name);
    }

    let matches = command.try_get_matches_from(args)?;
    cli::RunAliasCli::from_arg_matches(&matches)
}

fn dispatch_run_alias(cli: cli::RunAliasCli, dir: &Path) -> Result<i32> {
    let mut ctx = detect::detect(dir);
    let loaded_config = config::load(dir)?;
    if let Some(loaded) = &loaded_config {
        ctx.warnings.extend(loaded.warnings.iter().cloned());
    }
    let overrides = resolver::ResolutionOverrides::from_cli_and_env(
        cli.global.pm_override.as_deref(),
        cli.global.runner_override.as_deref(),
        cli.global.fallback.as_deref(),
        cli.global.on_mismatch.as_deref(),
        resolver::DiagnosticFlags {
            no_warnings: cli.global.no_warnings,
            quiet: cli.global.quiet,
            explain: cli.global.explain,
        },
        cli::ChainFailureFlags {
            keep_going: cli.failure.keep_going,
            kill_on_fail: cli.failure.kill_on_fail,
        },
        loaded_config.as_ref(),
    )?;
    match cli.task {
        None if !cli.mode.sequential && !cli.mode.parallel => {
            cmd::info(&ctx, &overrides, false, schema::CURRENT_VERSION)?;
            Ok(0)
        }
        task => dispatch_run(&ctx, &overrides, task, cli.args, cli.mode),
    }
}

/// Extracts the filename portion from an `argv[0]`-style `OsString`, returning it when non-empty.
///
/// Returns `Some(String)` with the file name if `arg0` has a non-empty file-name segment, `None` otherwise.
///
/// Strips a trailing `.exe` suffix (case-insensitive) so Windows builds present the
/// same `runner` / `run` identifier in `--version`, `--help`, and the `Usage:` line
/// as Unix builds. Without this, clap's bin-name plumbing surfaces the raw
/// `runner.exe` from `argv[0]`, leaking the platform-specific extension into UX.
///
/// # Examples
///
/// ```rust
/// use std::ffi::OsString;
/// let name = runner::bin_name_from_arg0(&OsString::from("/usr/bin/runner"));
/// assert_eq!(name.as_deref(), Some("runner"));
///
/// let win = runner::bin_name_from_arg0(&OsString::from("runner.exe"));
/// assert_eq!(win.as_deref(), Some("runner"));
/// ```
#[must_use]
pub fn bin_name_from_arg0(arg0: &OsString) -> Option<String> {
    let name = Path::new(arg0)
        .file_name()
        .map(|segment| segment.to_string_lossy().into_owned())?;

    let trimmed = strip_exe_suffix(&name);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Strip a trailing `.exe` extension (ASCII case-insensitive) from a file name.
///
/// Returns the input unchanged if no such suffix is present. The match is
/// ASCII-only because Windows treats `.EXE`, `.Exe`, `.exe` etc. as the same
/// extension, and that case-fold is bounded to ASCII regardless of the active
/// code page.
fn strip_exe_suffix(name: &str) -> &str {
    const SUFFIX: &str = ".exe";
    if name.len() > SUFFIX.len()
        && name.is_char_boundary(name.len() - SUFFIX.len())
        && name[name.len() - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX)
    {
        &name[..name.len() - SUFFIX.len()]
    } else {
        name
    }
}

/// Attaches the generated help byline to a clap command.
///
/// The byline text is produced by `help_byline` using `stdout_is_terminal` and is
/// applied via `Command::before_help`.
///
/// # Examples
///
/// ```rust
/// let cmd = clap::Command::new("app");
/// let cmd = runner::configure_cli_command(cmd, true);
/// assert!(cmd.get_before_help().is_some());
/// ```
#[must_use]
pub fn configure_cli_command(command: clap::Command, stdout_is_terminal: bool) -> clap::Command {
    command.before_help(help_byline(stdout_is_terminal))
}

/// Render the CLI help byline using the build-time author metadata.
///
/// When `stdout_is_terminal` is true and `RUNNER_AUTHOR_EMAIL` is set, the
/// author name is wrapped in an OSC-8 `mailto:` hyperlink; otherwise the plain
/// author name is used. The returned string is prefixed with `"by "`.
///
/// # Examples
///
/// ```rust
/// // Without a terminal, output is plain "by <name>" using the build-time author.
/// let s = runner::help_byline(false);
/// assert!(s.starts_with("by "));
///
/// // With a terminal, the name may be wrapped in an OSC-8 mailto: hyperlink,
/// // but the byline still begins with "by ".
/// let t = runner::help_byline(true);
/// assert!(t.starts_with("by "));
/// ```
#[must_use]
pub fn help_byline(stdout_is_terminal: bool) -> String {
    let name = env!("RUNNER_AUTHOR_NAME");
    let rendered = if stdout_is_terminal {
        option_env!("RUNNER_AUTHOR_EMAIL").map_or_else(
            || name.to_string(),
            |mail| osc8_link(name, &format!("mailto:{mail}")),
        )
    } else {
        name.to_string()
    };
    format!("by {rendered}")
}

/// Detects whether the provided argv-style slice specifically requests the program version.
///
/// # Returns
///
/// `true` if `args` has exactly two elements and the second element is `--version` or `-V`, `false` otherwise.
///
/// # Examples
///
/// ```rust
/// use std::ffi::OsString;
///
/// let args = vec![OsString::from("runner"), OsString::from("--version")];
/// assert!(runner::requests_version(&args));
///
/// let args2 = vec![OsString::from("runner"), OsString::from("-V")];
/// assert!(runner::requests_version(&args2));
///
/// let args3 = vec![OsString::from("runner")];
/// assert!(!runner::requests_version(&args3));
///
/// let args4 = vec![OsString::from("runner"), OsString::from("--version"), OsString::from("extra")];
/// assert!(!runner::requests_version(&args4));
/// ```
#[must_use]
pub fn requests_version(args: &[OsString]) -> bool {
    if args.len() != 2 {
        return false;
    }

    let flag = args[1].to_string_lossy();
    flag == "--version" || flag == "-V"
}

fn version_line(args: &[OsString], stdout_is_terminal: bool) -> String {
    let bin = args
        .first()
        .and_then(bin_name_from_arg0)
        .unwrap_or_else(|| "runner".to_string());

    if !stdout_is_terminal {
        return format!("{bin} {VERSION}");
    }

    format!(
        "{} {}",
        osc8_link(&bin, REPOSITORY_URL),
        osc8_link(VERSION, &release_url(VERSION))
    )
}

fn release_url(version: &str) -> String {
    format!("{REPOSITORY_URL}releases/tag/v{version}")
}

fn osc8_link(label: &str, url: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{label}\u{1b}]8;;\u{1b}\\")
}

fn configured_project_dir(
    project_dir: Option<&Path>,
    env_dir: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    project_dir
        .map(Path::to_path_buf)
        .or_else(|| env_dir.map(PathBuf::from))
}

/// Expand a leading `~` (or `~/`) in a path to the user's home directory.
///
/// Shells only expand an unquoted tilde when it is the first character of a
/// word, so forms like `--dir=~/foo` arrive here unexpanded. We mirror the
/// common shell behaviour for the bare `~` and `~/` cases; any other form
/// (including `~user`) is returned unchanged.
fn expand_tilde(path: &Path) -> PathBuf {
    expand_tilde_with(path, home_dir().as_deref())
}

fn expand_tilde_with(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };

    match path.strip_prefix("~") {
        // `~` on its own.
        Ok(rest) if rest.as_os_str().is_empty() => home.to_path_buf(),
        // `~/rest` (`strip_prefix` consumes the separator).
        Ok(rest) => home.join(rest),
        // Not a tilde path, or a form we don't expand (e.g. `~user`).
        Err(_) => path.to_path_buf(),
    }
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn resolve_project_dir(project_dir: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let project_dir = project_dir.map(expand_tilde);
    let dir = match project_dir.as_deref() {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => cwd.join(path),
        None => cwd.to_path_buf(),
    };

    if !dir.exists() {
        bail!("project dir does not exist: {}", dir.display());
    }
    if !dir.is_dir() {
        bail!("project dir is not a directory: {}", dir.display());
    }

    Ok(dir)
}

fn render_clap_error(err: &clap::Error) -> Result<i32> {
    let exit_code = err.exit_code();
    err.print()?;
    Ok(exit_code)
}

fn dispatch_install_chain(
    ctx: &types::ProjectContext,
    overrides: &resolver::ResolutionOverrides,
    frozen: bool,
    tasks: &[String],
) -> Result<i32> {
    let mut items = vec![chain::ChainItem::install(frozen)];
    items.extend(chain::parse::parse_task_list(tasks)?);
    let c = chain::Chain {
        mode: chain::ChainMode::Sequential,
        items,
        failure: overrides.failure_policy,
    };
    chain::exec::run_chain(ctx, overrides, &c)
}

fn dispatch_run(
    ctx: &types::ProjectContext,
    overrides: &resolver::ResolutionOverrides,
    task: Option<String>,
    args: Vec<String>,
    mode: cli::ChainModeFlags,
) -> Result<i32> {
    if mode.sequential || mode.parallel {
        let chain_mode = if mode.parallel {
            chain::ChainMode::Parallel
        } else {
            chain::ChainMode::Sequential
        };
        let mut positionals: Vec<String> = Vec::new();
        if let Some(t) = task {
            positionals.push(t);
        }
        positionals.extend(args);
        let items = chain::parse::parse_task_list(&positionals)?;
        let c = chain::Chain {
            mode: chain_mode,
            items,
            failure: overrides.failure_policy,
        };
        return chain::exec::run_chain(ctx, overrides, &c);
    }
    let Some(task) = task.as_deref() else {
        bail!(
            "task name required (drop -s/-p for single-task mode or supply at least one task name)"
        );
    };
    if args.is_empty()
        && let Some(code) = run_path_builtin_fallback(ctx, overrides, task)?
    {
        return Ok(code);
    }
    cmd::run(ctx, overrides, task, &args, None)
}

/// Run-path fallback for builtin verbs.
///
/// When a bare, arg-less `run`/`runner run` token names a built-in verb and
/// no same-named task exists, run that built-in's default (no-flag) form —
/// the same behavior the explicit `runner <verb>` subcommand provides. A
/// project task of the same name takes precedence (handled by the early
/// `has_task` return → falls through to `cmd::run`).
///
/// Returns `Ok(Some(code))` when the fallback handled the token, `Ok(None)`
/// to fall through to `cmd::run` (task dispatch / PM-exec).
///
/// Qualified tokens (`source:verb`) carry the `source:` prefix, so they never
/// match a bare verb arm and fall through untouched — no qualifier parsing
/// needed here. `info` maps to a plain `list` (no deprecation warning): the
/// deprecation is specific to the explicit `runner info` subcommand, and
/// emitting it on the run path — where the user typed `run info` — would be
/// misleading and would spuriously fire the GitHub Actions annotation.
fn run_path_builtin_fallback(
    ctx: &types::ProjectContext,
    overrides: &resolver::ResolutionOverrides,
    name: &str,
) -> Result<Option<i32>> {
    if has_task(ctx, name) {
        return Ok(None);
    }
    let code = match name {
        "install" => cmd::install(ctx, overrides, false)?,
        "clean" => {
            cmd::clean(ctx, false, false)?;
            0
        }
        // `info` maps to a plain `list`: the deprecation warning is specific
        // to the explicit `runner info` subcommand, not the run path.
        "list" | "info" => {
            cmd::list(ctx, overrides, false, false, None, schema::CURRENT_VERSION)?;
            0
        }
        "completions" => {
            cmd::completions(None, None)?;
            0
        }
        _ => return Ok(None),
    };
    Ok(Some(code))
}

/// Resolve the effective JSON schema version for schema-aware output:
/// explicit `--schema-version=N` wins, otherwise default to latest.
fn resolve_schema_version(requested: Option<u32>) -> Result<u32> {
    schema::validate_schema_version(requested.unwrap_or(schema::CURRENT_VERSION))
}

fn schema_version_for_json(json: bool, requested: Option<u32>) -> Result<u32> {
    if json {
        resolve_schema_version(requested)
    } else {
        Ok(schema::CURRENT_VERSION)
    }
}

/// `why`-specific version resolution: `why` is at
/// [`schema::WHY_CURRENT_VERSION`] while list remains at
/// [`schema::CURRENT_VERSION`], so it validates against its own range
/// and defaults to its own latest.
fn why_schema_version_for_json(json: bool, requested: Option<u32>) -> Result<u32> {
    if json {
        schema::validate_why_schema_version(requested.unwrap_or(schema::WHY_CURRENT_VERSION))
    } else {
        Ok(schema::WHY_CURRENT_VERSION)
    }
}

/// `doctor`-specific version resolution; see
/// [`schema::DOCTOR_CURRENT_VERSION`].
fn doctor_schema_version_for_json(json: bool, requested: Option<u32>) -> Result<u32> {
    if json {
        schema::validate_doctor_schema_version(requested.unwrap_or(schema::DOCTOR_CURRENT_VERSION))
    } else {
        Ok(schema::DOCTOR_CURRENT_VERSION)
    }
}

/// Build [`resolver::ResolutionOverrides`] from a parsed CLI + loaded config.
/// Lifted out of [`dispatch`] so the latter stays under clippy's
/// `too_many_lines` budget; the chain-failure inputs come from whichever
/// subcommand carries them (`Run` / `Install`), with `false` defaults for
/// subcommands that don't.
fn build_overrides(
    cli: &cli::Cli,
    loaded_config: Option<&config::LoadedConfig>,
) -> Result<resolver::ResolutionOverrides> {
    let (cli_keep_going, cli_kill_on_fail) = match cli.command.as_ref() {
        Some(cli::Command::Run { failure, .. } | cli::Command::Install { failure, .. }) => {
            (failure.keep_going, failure.kill_on_fail)
        }
        _ => (false, false),
    };
    resolver::ResolutionOverrides::from_cli_and_env(
        cli.global.pm_override.as_deref(),
        cli.global.runner_override.as_deref(),
        cli.global.fallback.as_deref(),
        cli.global.on_mismatch.as_deref(),
        resolver::DiagnosticFlags {
            no_warnings: cli.global.no_warnings,
            quiet: cli.global.quiet,
            explain: cli.global.explain,
        },
        cli::ChainFailureFlags {
            keep_going: cli_keep_going,
            kill_on_fail: cli_kill_on_fail,
        },
        loaded_config,
    )
}

/// Lenient sibling of [`build_overrides`] used when strict parsing
/// failed and the command is `doctor`: invalid env-sourced override
/// values degrade to [`types::DetectionWarning`]s instead of killing
/// the one command whose job is to report a broken environment.
fn build_overrides_lenient(
    cli: &cli::Cli,
    loaded_config: Option<&config::LoadedConfig>,
) -> Result<(resolver::ResolutionOverrides, Vec<types::DetectionWarning>)> {
    let (cli_keep_going, cli_kill_on_fail) = match cli.command.as_ref() {
        Some(cli::Command::Run { failure, .. } | cli::Command::Install { failure, .. }) => {
            (failure.keep_going, failure.kill_on_fail)
        }
        _ => (false, false),
    };
    resolver::ResolutionOverrides::from_cli_and_env_lenient(
        cli.global.pm_override.as_deref(),
        cli.global.runner_override.as_deref(),
        cli.global.fallback.as_deref(),
        cli.global.on_mismatch.as_deref(),
        resolver::DiagnosticFlags {
            no_warnings: cli.global.no_warnings,
            quiet: cli.global.quiet,
            explain: cli.global.explain,
        },
        cli::ChainFailureFlags {
            keep_going: cli_keep_going,
            kill_on_fail: cli_kill_on_fail,
        },
        loaded_config,
    )
}

/// Resolve overrides for [`dispatch`]. Strict for every command;
/// `doctor` retries leniently on failure because it must survive the
/// misconfigured environment it exists to diagnose — env garbage
/// degrades to warnings appended to `ctx`, while CLI flag garbage
/// re-raises from the lenient pass and stays fatal.
fn dispatch_overrides(
    cli: &cli::Cli,
    loaded_config: Option<&config::LoadedConfig>,
    ctx: &mut types::ProjectContext,
) -> Result<resolver::ResolutionOverrides> {
    match build_overrides(cli, loaded_config) {
        Ok(overrides) => Ok(overrides),
        Err(_) if matches!(cli.command, Some(cli::Command::Doctor { .. })) => {
            let (overrides, env_warnings) = build_overrides_lenient(cli, loaded_config)?;
            ctx.warnings.extend(env_warnings);
            Ok(overrides)
        }
        Err(e) => Err(e),
    }
}

fn dispatch(cli: cli::Cli, dir: &Path) -> Result<i32> {
    let mut ctx = detect::detect(dir);
    // A malformed `runner.toml` must not abort the `config` subcommand —
    // `config validate`/`show` exist to inspect and repair exactly that
    // file, and they re-load it with their own error handling. Unknown
    // sections/fields are tolerated everywhere (forward compat) and surface
    // as warnings; only an unreadable/syntactically-broken file or a
    // wrong-typed known field still fails the parse here.
    let loaded_config = match config::load(dir) {
        Ok(loaded) => loaded,
        Err(_) if matches!(cli.command, Some(cli::Command::Config { .. })) => None,
        Err(e) => return Err(e),
    };
    if let Some(loaded) = &loaded_config {
        ctx.warnings.extend(loaded.warnings.iter().cloned());
    }
    let overrides = dispatch_overrides(&cli, loaded_config.as_ref(), &mut ctx)?;

    match cli.command {
        None => {
            cmd::info(&ctx, &overrides, false, schema::CURRENT_VERSION)?;
            Ok(0)
        }
        // `info` is a deprecated alias for `list`. Bare `runner` (the
        // `None` arm above) keeps the dashboard; only the explicit verb
        // is deprecated.
        Some(cli::Command::Info { json }) => {
            eprintln!(
                "{} `runner info` is deprecated; use `runner list`",
                "warn:".yellow().bold(),
            );
            // Under GitHub Actions, also emit a workflow-command
            // annotation so the deprecation surfaces in the run summary
            // / inline, not just buried in the step log. Kept on stderr
            // so `runner info --json` stdout stays a clean pipe; the
            // runner scans both streams for `::` commands.
            if actions_rs::env::is_github_actions() {
                eprintln!(
                    "::warning title=Deprecation::`runner info` is deprecated; use `runner list`"
                );
            }
            let schema_version = schema_version_for_json(json, cli.global.schema_version)?;
            cmd::list(&ctx, &overrides, false, json, None, schema_version)?;
            Ok(0)
        }
        Some(cli::Command::Run {
            task, args, mode, ..
        }) => dispatch_run(&ctx, &overrides, task, args, mode),
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                cmd::info(&ctx, &overrides, false, schema::CURRENT_VERSION)?;
                Ok(0)
            } else {
                cmd::run(&ctx, &overrides, &args[0], &args[1..], None)
            }
        }
        Some(cli::Command::Install { frozen, tasks, .. }) if !tasks.is_empty() => {
            dispatch_install_chain(&ctx, &overrides, frozen, &tasks)
        }
        Some(cli::Command::Install { frozen, .. }) => cmd::install(&ctx, &overrides, frozen),
        Some(cli::Command::Clean {
            yes,
            include_framework,
        }) => {
            cmd::clean(&ctx, yes, include_framework)?;
            Ok(0)
        }
        Some(cli::Command::List { raw, json, source }) => {
            let schema_version = schema_version_for_json(json, cli.global.schema_version)?;
            cmd::list(
                &ctx,
                &overrides,
                raw,
                json,
                source.as_deref(),
                schema_version,
            )?;
            Ok(0)
        }
        Some(cli::Command::Completions { shell, output }) => {
            cmd::completions(shell, output.as_deref())?;
            Ok(0)
        }
        #[cfg(feature = "man")]
        Some(cli::Command::Man { output }) => dispatch_man(output.as_deref()),
        #[cfg(feature = "schema")]
        Some(cli::Command::Schema { all, output }) => dispatch_schema(all, output.as_deref()),
        Some(cli::Command::Doctor { json }) => {
            let schema_version = doctor_schema_version_for_json(json, cli.global.schema_version)?;
            cmd::doctor(&ctx, &overrides, json, schema_version)?;
            Ok(0)
        }
        Some(cli::Command::Config { action }) => cmd::config(dir, action),
        Some(cli::Command::Why { task, json }) => {
            let schema_version = why_schema_version_for_json(json, cli.global.schema_version)?;
            cmd::why(&ctx, &overrides, &task, json, schema_version)?;
            Ok(0)
        }
    }
}

#[cfg(feature = "man")]
fn dispatch_man(output: Option<&Path>) -> Result<i32> {
    match output {
        Some(dir) => cmd::write_man_pages(dir)?,
        None => cmd::write_runner_page_to_stdout()?,
    }
    Ok(0)
}

#[cfg(feature = "schema")]
fn dispatch_schema(all: bool, output: Option<&Path>) -> Result<i32> {
    cmd::write_schema(all, output)?;
    Ok(0)
}

/// Whether the detected project defines a task with the given name.
fn has_task(ctx: &types::ProjectContext, name: &str) -> bool {
    ctx.tasks.iter().any(|task| task.name == name)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{
        AliasBuiltin, VERSION, alias_builtin_request, bin_name_from_arg0, configured_project_dir,
        exit_code_for_error, expand_tilde_with, has_task, parse_cli, parse_run_alias_cli,
        release_url, requests_version, resolve_project_dir, run_alias_in_dir, run_in_dir,
        version_line,
    };
    use crate::cli;
    use crate::resolver::ResolveError;
    use crate::tool::test_support::TempDir;
    use crate::types::{Ecosystem, ProjectContext, Task, TaskSource};

    #[test]
    fn exit_code_for_resolve_error_is_two() {
        let err: anyhow::Error = ResolveError::NoSignalsFound {
            ecosystem: Ecosystem::Node,
            soft: false,
        }
        .into();

        assert_eq!(exit_code_for_error(&err), 2);
    }

    #[test]
    fn exit_code_for_generic_error_is_one() {
        let err = anyhow::anyhow!("generic boom");

        assert_eq!(exit_code_for_error(&err), 1);
    }

    #[test]
    fn help_returns_zero_instead_of_exiting() {
        let code = run_in_dir(["runner", "--help"], Path::new("."))
            .expect("help should return an exit code");

        assert_eq!(code, 0);
    }

    #[test]
    fn invalid_args_return_non_zero_instead_of_exiting() {
        let code = run_in_dir(["runner", "--definitely-invalid"], Path::new("."))
            .expect("parse errors should return an exit code");

        assert_ne!(code, 0);
    }

    #[test]
    fn version_returns_zero_instead_of_exiting() {
        let code = run_in_dir(["runner", "--version"], Path::new("."))
            .expect("version should return an exit code");

        assert_eq!(code, 0);
    }

    #[test]
    fn requests_version_detects_top_level_version_flags() {
        assert!(requests_version(&[
            OsString::from("runner"),
            OsString::from("--version")
        ]));
        assert!(requests_version(&[
            OsString::from("runner"),
            OsString::from("-V")
        ]));
        assert!(!requests_version(&[
            OsString::from("runner"),
            OsString::from("info"),
            OsString::from("--version"),
        ]));
    }

    #[test]
    fn release_url_points_to_version_tag() {
        assert_eq!(
            release_url(VERSION),
            format!("https://github.com/kjanat/runner/releases/tag/v{VERSION}")
        );
    }

    #[test]
    fn version_line_wraps_bin_and_version_with_separate_links() {
        let line = version_line(&[OsString::from("runner")], true);

        assert!(line.contains(
            "\u{1b}]8;;https://github.com/kjanat/runner/\u{1b}\\runner\u{1b}]8;;\u{1b}\\"
        ));
        assert!(line.contains(&format!(
            "\u{1b}]8;;https://github.com/kjanat/runner/releases/tag/v{VERSION}\u{1b}\\{VERSION}\u{1b}]8;;\u{1b}\\"
        )));
    }

    #[test]
    fn resolve_project_dir_uses_cwd_when_not_overridden() {
        let cwd = TempDir::new("runner-project-dir-default");

        assert_eq!(
            resolve_project_dir(None, cwd.path()).expect("cwd should be accepted"),
            cwd.path()
        );
    }

    #[test]
    fn resolve_project_dir_resolves_relative_paths_from_cwd() {
        let cwd = TempDir::new("runner-project-dir-cwd");
        fs::create_dir(cwd.path().join("child")).expect("child dir should be created");

        let resolved = resolve_project_dir(Some(Path::new("child")), cwd.path())
            .expect("relative dir should resolve");

        assert_eq!(resolved, cwd.path().join("child"));
    }

    #[test]
    fn resolve_project_dir_rejects_missing_directories() {
        let cwd = TempDir::new("runner-project-dir-missing");
        let err = resolve_project_dir(Some(Path::new("missing")), cwd.path())
            .expect_err("missing dir should error");

        assert!(err.to_string().contains("project dir does not exist"));
    }

    #[test]
    fn expand_tilde_expands_leading_tilde_slash() {
        let home = Path::new("/home/example");
        assert_eq!(
            expand_tilde_with(Path::new("~/projects/recipe"), Some(home)),
            home.join("projects/recipe"),
        );
    }

    #[test]
    fn expand_tilde_expands_bare_tilde() {
        let home = Path::new("/home/example");
        assert_eq!(expand_tilde_with(Path::new("~"), Some(home)), home);
    }

    #[test]
    fn expand_tilde_leaves_other_paths_untouched() {
        let home = Path::new("/home/example");
        for raw in ["/abs/path", "relative/path", "~user/projects", "./~/foo"] {
            assert_eq!(
                expand_tilde_with(Path::new(raw), Some(home)),
                PathBuf::from(raw),
                "path {raw} should be unchanged",
            );
        }
    }

    #[test]
    fn expand_tilde_without_home_is_noop() {
        assert_eq!(
            expand_tilde_with(Path::new("~/projects"), None),
            PathBuf::from("~/projects"),
        );
    }

    #[test]
    fn resolve_project_dir_does_not_join_tilde_onto_cwd() {
        // Regression: `--dir=~/foo` arrives unexpanded, and previously the
        // tilde path was treated as relative and joined onto the cwd, yielding
        // a bogus `<cwd>/~/foo`. The cwd exists but `<cwd>/~/foo` must not, so
        // a non-expanding implementation would fail with a path containing the
        // literal tilde segment.
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        if std::env::var_os(home_var).is_none_or(|v| v.is_empty()) {
            // Without a home directory there is nothing to expand to; the pure
            // `expand_tilde_with` tests cover the no-home path instead.
            return;
        }

        let cwd = TempDir::new("runner-project-dir-tilde");
        let err = resolve_project_dir(Some(Path::new("~/definitely-missing")), cwd.path())
            .expect_err("tilde dir should not resolve against cwd");

        let message = err.to_string();
        assert!(message.contains("project dir does not exist"));
        assert!(
            !message.contains(&format!("{}/~", cwd.path().display())),
            "tilde must not be joined onto cwd: {message}",
        );
    }

    #[test]
    fn configured_project_dir_prefers_flag_over_env() {
        let dir = configured_project_dir(
            Some(Path::new("flag-dir")),
            Some(std::ffi::OsStr::new("env-dir")),
        )
        .expect("dir should be selected");

        assert_eq!(dir, PathBuf::from("flag-dir"));
    }

    #[test]
    fn configured_project_dir_falls_back_to_env() {
        let dir = configured_project_dir(None, Some(std::ffi::OsStr::new("env-dir")))
            .expect("env dir should be selected");

        assert_eq!(dir, PathBuf::from("env-dir"));
    }

    #[test]
    fn bin_name_from_arg0_uses_path_file_name() {
        let name = bin_name_from_arg0(&OsString::from("/tmp/run"));

        assert_eq!(name.as_deref(), Some("run"));
    }

    #[test]
    fn bin_name_from_arg0_strips_windows_exe_suffix() {
        // Windows builds inherit `runner.exe` / `run.exe` from argv[0]; clap
        // pipes that straight into `--version` / `--help` / Usage unless we
        // normalize it here. We feed bare file names rather than full Windows
        // paths because `Path::file_name` is host-OS-aware and won't split on
        // `\` when the tests run on Unix.
        let runner = bin_name_from_arg0(&OsString::from("runner.exe"));
        assert_eq!(runner.as_deref(), Some("runner"));

        let run = bin_name_from_arg0(&OsString::from("run.exe"));
        assert_eq!(run.as_deref(), Some("run"));
    }

    #[test]
    fn bin_name_from_arg0_strips_exe_case_insensitive() {
        let upper = bin_name_from_arg0(&OsString::from("RUNNER.EXE"));
        assert_eq!(upper.as_deref(), Some("RUNNER"));

        let mixed = bin_name_from_arg0(&OsString::from("Run.Exe"));
        assert_eq!(mixed.as_deref(), Some("Run"));
    }

    #[test]
    fn bin_name_from_arg0_preserves_unrelated_extensions() {
        // `.exe` only — names that happen to embed those characters in other
        // positions, or carry different extensions, pass through unchanged.
        let dotted = bin_name_from_arg0(&OsString::from("/tmp/runner.exe.bak"));
        assert_eq!(dotted.as_deref(), Some("runner.exe.bak"));

        let other = bin_name_from_arg0(&OsString::from("/tmp/runner.sh"));
        assert_eq!(other.as_deref(), Some("runner.sh"));
    }

    #[test]
    fn bin_name_from_arg0_handles_bare_dot_exe() {
        // `.exe` alone shouldn't strip to an empty name; the suffix length
        // guard keeps the input intact.
        let bare = bin_name_from_arg0(&OsString::from(".exe"));
        assert_eq!(bare.as_deref(), Some(".exe"));
    }

    fn stub_context(tasks: &[&str]) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers: Vec::new(),
            task_runners: Vec::new(),
            tasks: tasks
                .iter()
                .map(|name| Task {
                    name: (*name).to_string(),
                    source: TaskSource::PackageJson,
                    run_target: None,
                    description: None,
                    alias_of: None,
                    passthrough_to: None,
                })
                .collect(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn has_task_returns_true_for_existing_task() {
        let ctx = stub_context(&["clean", "install"]);

        assert!(has_task(&ctx, "clean"));
        assert!(has_task(&ctx, "install"));
        assert!(!has_task(&ctx, "build"));
    }

    #[test]
    fn run_alias_parses_builtin_names_as_tasks() {
        for name in [
            "clean",
            "install",
            "list",
            "exec",
            "info",
            "completions",
            "run",
        ] {
            let cli = parse_run_alias_cli(["run", name])
                .unwrap_or_else(|e| panic!("run {name} should parse: {e}"));

            assert_eq!(cli.task.as_deref(), Some(name));
            assert!(cli.args.is_empty());
        }
    }

    #[test]
    fn run_alias_forwards_trailing_args() {
        let cli = parse_run_alias_cli(["run", "test", "--watch", "--reporter=verbose"])
            .expect("run test --watch --reporter=verbose should parse");

        assert_eq!(cli.task.as_deref(), Some("test"));
        assert_eq!(cli.args, vec!["--watch", "--reporter=verbose"]);
    }

    #[test]
    fn run_alias_bare_has_no_task() {
        let cli = parse_run_alias_cli(["run"]).expect("bare run should parse");

        assert!(cli.task.is_none());
        assert!(cli.args.is_empty());
    }

    #[test]
    fn run_alias_honours_dir_flag() {
        let cli = parse_run_alias_cli(["run", "--dir=other", "build"])
            .expect("run --dir=other build should parse");

        assert_eq!(cli.global.project_dir, Some(PathBuf::from("other")));
        assert_eq!(cli.task.as_deref(), Some("build"));
    }

    #[test]
    fn run_alias_bare_shows_info() {
        let dir = TempDir::new("runner-run-bare");

        let code =
            run_alias_in_dir(["run"], dir.path()).expect("bare run should succeed on empty dir");

        assert_eq!(code, 0);
    }

    #[test]
    fn run_alias_forwards_help_and_version_after_task() {
        // `run <task> --help/--version` must reach the task, not print
        // run's own help/version. The flag is an undefined hyphen token
        // after the first positional, so `args` (trailing_var_arg) keeps it.
        for flag in ["--help", "-h", "--version", "-V"] {
            let cli = parse_run_alias_cli(["run", "build", flag])
                .unwrap_or_else(|e| panic!("run build {flag} should parse: {e}"));
            assert_eq!(cli.task.as_deref(), Some("build"));
            assert_eq!(cli.args, vec![flag.to_string()]);
        }
    }

    #[test]
    fn run_alias_forwards_interleaved_help_flag() {
        // A forwarded help flag keeps its position among the task's args.
        let cli = parse_run_alias_cli(["run", "build", "--foo", "--help", "--bar"])
            .expect("interleaved --help should parse and forward");
        assert_eq!(cli.task.as_deref(), Some("build"));
        assert_eq!(cli.args, vec!["--foo", "--help", "--bar"]);
    }

    #[test]
    fn run_alias_double_dash_forwards_help_literally() {
        // `run <task> -- --help` keeps forwarding the literal flag (the `--`
        // separator itself is consumed by clap).
        let cli = parse_run_alias_cli(["run", "build", "--", "--help"])
            .expect("run build -- --help should parse");
        assert_eq!(cli.task.as_deref(), Some("build"));
        assert_eq!(cli.args, vec!["--help"]);
    }

    #[test]
    fn run_alias_leading_builtins_classified_as_own_request() {
        // Before any task, a help/version flag can't fill the
        // hyphen-rejecting `task` positional (clap built-ins are disabled),
        // so it surfaces as UnknownArgument and is recognised as ours.
        for flag in ["--help", "-h"] {
            let err = parse_run_alias_cli(["run", flag])
                .expect_err("leading help flag should not parse as a task");
            assert!(
                matches!(alias_builtin_request(&err), Some(AliasBuiltin::Help)),
                "{flag} before a task should be classified as a help request",
            );
        }
        for flag in ["--version", "-V"] {
            let err = parse_run_alias_cli(["run", flag])
                .expect_err("leading version flag should not parse as a task");
            assert!(
                matches!(alias_builtin_request(&err), Some(AliasBuiltin::Version)),
                "{flag} before a task should be classified as a version request",
            );
        }
    }

    #[test]
    fn run_alias_global_flag_before_help_still_classified_as_help() {
        // `run --pm npm --help`: the value-taking global flag is consumed,
        // then --help still lands before any task → run's own help.
        let err = parse_run_alias_cli(["run", "--pm", "npm", "--help"])
            .expect_err("--pm npm --help should not parse as a task");
        assert!(matches!(
            alias_builtin_request(&err),
            Some(AliasBuiltin::Help)
        ));
    }

    #[test]
    fn run_alias_unknown_flag_is_not_a_builtin_request() {
        // A genuine unknown flag must surface as an error, never be
        // mistaken for a help/version request.
        let err = parse_run_alias_cli(["run", "--bogus"])
            .expect_err("unknown leading flag should not parse");
        assert!(alias_builtin_request(&err).is_none());
    }

    #[test]
    fn run_alias_own_help_and_version_return_zero() {
        // End-to-end through dispatch: own help/version exit 0 without
        // needing a real project. `--pm npm --version` is len > 2 so it
        // bypasses the `requests_version` fast-path and exercises the
        // parse-error classification.
        let dir = TempDir::new("runner-run-builtin");
        assert_eq!(
            run_alias_in_dir(["run", "--help"], dir.path()).expect("run --help should succeed"),
            0,
        );
        assert_eq!(
            run_alias_in_dir(["run", "--pm", "npm", "--version"], dir.path())
                .expect("run --pm npm --version should succeed"),
            0,
        );
    }

    #[test]
    fn runner_cli_still_parses_install_as_builtin_when_flag_set() {
        let cli = parse_cli(["runner", "install", "--frozen"]).expect("should parse");

        match cli.command {
            Some(cli::Command::Install { frozen: true, .. }) => {}
            other => panic!("expected Install {{ frozen: true }}, got {other:?}"),
        }
    }

    #[test]
    fn runner_cli_parses_install_frozen_short_flag() {
        let cli = parse_cli(["runner", "install", "-f"]).expect("should parse");

        match cli.command {
            Some(cli::Command::Install { frozen: true, .. }) => {}
            other => panic!("expected Install {{ frozen: true }}, got {other:?}"),
        }
    }

    #[test]
    fn runner_cli_parses_install_chain_flags_after_task_names() {
        // `runner install build test --kill-on-fail` must parse
        // `--kill-on-fail` as a chain-failure flag, not as a task name.
        // Regression for the `trailing_var_arg` consumption bug.
        let cli = parse_cli(["runner", "install", "build", "test", "-K"]).expect("parses");
        match cli.command {
            Some(cli::Command::Install {
                tasks,
                failure:
                    cli::ChainFailureFlags {
                        kill_on_fail: true, ..
                    },
                ..
            }) => assert_eq!(tasks, vec!["build".to_string(), "test".to_string()]),
            other => {
                panic!("expected Install with kill_on_fail=true and clean task list, got {other:?}")
            }
        }
    }

    #[test]
    fn runner_cli_parses_clean_as_builtin_when_flag_set() {
        let cli = parse_cli(["runner", "clean", "-y"]).expect("should parse");

        match cli.command {
            Some(cli::Command::Clean { yes: true, .. }) => {}
            other => panic!("expected Clean {{ yes: true, .. }}, got {other:?}"),
        }
    }

    #[test]
    fn runner_cli_routes_unknown_name_to_external() {
        let cli = parse_cli(["runner", "no-such-builtin"]).expect("should parse");

        match cli.command {
            Some(cli::Command::External(args)) => {
                assert_eq!(args, vec!["no-such-builtin"]);
            }
            other => panic!("expected External, got {other:?}"),
        }
    }

    #[test]
    fn runner_cli_parses_pm_and_runner_overrides_globally() {
        let cli = parse_cli(["runner", "--pm", "pnpm", "--runner", "just", "run", "build"])
            .expect("global --pm/--runner should parse on the run subcommand");

        assert_eq!(cli.global.pm_override.as_deref(), Some("pnpm"));
        assert_eq!(cli.global.runner_override.as_deref(), Some("just"));
        match cli.command {
            Some(cli::Command::Run { task, args, .. }) => {
                assert_eq!(task.as_deref(), Some("build"));
                assert!(args.is_empty());
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_alias_parses_pm_override() {
        let cli =
            parse_run_alias_cli(["run", "--pm=bun", "test"]).expect("--pm=bun test should parse");

        assert_eq!(cli.global.pm_override.as_deref(), Some("bun"));
        assert_eq!(cli.task.as_deref(), Some("test"));
    }

    #[test]
    fn invalid_pm_override_value_returns_error() {
        // Bad PM name should not crash the binary; it should surface as an
        // error exit code so the user sees the message from `from_cli_and_env`.
        let dir = TempDir::new("runner-bad-pm");
        let result = run_in_dir(["runner", "--pm", "zoot", "info"], dir.path());

        let err = result.expect_err("unknown --pm should error");
        assert!(format!("{err}").contains("unknown package manager"));
    }

    #[test]
    fn install_with_undetected_pm_override_exits_2() {
        // A cargo-only project with `--pm npm`: the override can't be
        // honored, so install must refuse with a ResolveError (exit 2)
        // before spawning anything.
        let dir = TempDir::new("runner-install-undetected-pm");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        )
        .expect("write Cargo.toml");

        let err = run_in_dir(["runner", "--pm", "npm", "install"], dir.path())
            .expect_err("undetected --pm should refuse the install");

        assert_eq!(
            exit_code_for_error(&err),
            2,
            "ResolveError must map to exit 2"
        );
        let msg = format!("{err}");
        assert!(msg.contains("--pm"), "should name the source: {msg}");
        assert!(msg.contains("cargo"), "should list detected PMs: {msg}");
    }

    #[test]
    fn install_chain_with_undetected_pm_override_exits_2() {
        // Same refusal through the chain path (`runner install <task>`).
        let dir = TempDir::new("runner-install-chain-undetected-pm");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        )
        .expect("write Cargo.toml");

        let err = run_in_dir(["runner", "--pm", "npm", "install", "build"], dir.path())
            .expect_err("undetected --pm should refuse the install chain");

        assert_eq!(
            exit_code_for_error(&err),
            2,
            "ResolveError must map to exit 2"
        );
    }

    #[test]
    fn schema_version_rejects_invalid_for_non_json_commands() {
        let dir = TempDir::new("runner-schema-invalid-completions");

        let code = run_in_dir(
            ["runner", "--schema-version", "99", "completions", "bash"],
            dir.path(),
        )
        .expect("parse errors should return an exit code");

        assert_ne!(code, 0);
    }

    #[test]
    fn schema_version_rejects_invalid_for_run_alias_bare_info() {
        let dir = TempDir::new("runner-schema-invalid-run-alias");

        let code = run_alias_in_dir(["run", "--schema-version", "99"], dir.path())
            .expect("parse errors should return an exit code");

        assert_ne!(code, 0);
    }

    #[test]
    fn schema_version_rejects_invalid_for_json_output() {
        let dir = TempDir::new("runner-schema-json-invalid");

        let code = run_in_dir(
            ["runner", "--schema-version", "99", "info", "--json"],
            dir.path(),
        )
        .expect("parse errors should return an exit code");

        assert_ne!(code, 0);
    }

    #[test]
    fn runner_cli_parses_completions_output_long() {
        let cli = parse_cli(["runner", "completions", "--output", "/tmp/runner.zsh"])
            .expect("should parse");

        match cli.command {
            Some(cli::Command::Completions {
                shell: None,
                output: Some(path),
            }) => assert_eq!(path, PathBuf::from("/tmp/runner.zsh")),
            other => panic!("expected Completions with --output long form, got {other:?}"),
        }
    }

    #[test]
    fn runner_cli_parses_completions_output_short() {
        let cli =
            parse_cli(["runner", "completions", "-o", "/tmp/runner.zsh"]).expect("should parse");

        match cli.command {
            Some(cli::Command::Completions {
                shell: None,
                output: Some(path),
            }) => assert_eq!(path, PathBuf::from("/tmp/runner.zsh")),
            other => panic!("expected Completions with -o short form, got {other:?}"),
        }
    }

    #[test]
    fn runner_cli_parses_completions_shell_and_output() {
        let cli = parse_cli([
            "runner",
            "completions",
            "zsh",
            "--output",
            "/tmp/runner.zsh",
        ])
        .expect("should parse");

        match cli.command {
            Some(cli::Command::Completions {
                shell: Some(_),
                output: Some(path),
            }) => assert_eq!(path, PathBuf::from("/tmp/runner.zsh")),
            other => panic!("expected Completions with both shell and output set, got {other:?}"),
        }
    }
}
