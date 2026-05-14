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
//! run <task>          # alias binary: always task/exec, never a built-in
//! runner run <target> # explicit unified run: task → PM exec fallback
//! runner install      # install dependencies via detected PM
//! runner clean        # remove caches and build artifacts
//! runner list         # list available tasks from all sources
//! ```
// Generate docs with `cargo doc --document-private-items --open`.

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

use resolver::ResolveError;

/// Generate the JSON Schema for `runner.toml`.
///
/// Only exposed when the `schema-gen` feature is on; the `gen-schema`
/// example calls this to keep `RunnerConfig` and its inner section
/// structs `pub(crate)` permanently — no permanent public-API
/// expansion just to derive a schema once.
#[cfg(feature = "schema-gen")]
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

    let matches = command.try_get_matches_from(args)?;
    cli::Cli::from_arg_matches(&matches)
}

/// Parse process args as the `run` alias binary, detect the current dir,
/// dispatch, and return the exit code.
///
/// Always treats positional arguments as a task or command (routed through [`cmd::run`])
/// — built-in subcommand names are never parsed specially, so
/// `run clean`, `run install`, etc. run the corresponding task/command.
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

    let cli = match parse_run_alias_cli(args) {
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
    dispatch_run_alias(cli, &project_dir)
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
    let ctx = detect::detect(dir);
    let loaded_config = config::load(dir)?;
    let overrides = resolver::ResolutionOverrides::from_cli_and_env(
        cli.global.pm_override.as_deref(),
        cli.global.runner_override.as_deref(),
        cli.global.fallback.as_deref(),
        cli.global.on_mismatch.as_deref(),
        resolver::DiagnosticFlags {
            no_warnings: cli.global.no_warnings,
            explain: cli.global.explain,
        },
        cli::ChainFailureFlags {
            keep_going: cli.failure.keep_going,
            kill_on_fail: cli.failure.kill_on_fail,
        },
        loaded_config.as_ref(),
    )?;
    let schema_version = resolve_schema_version(cli.global.schema_version)?;
    if cli.mode.sequential || cli.mode.parallel {
        let mode = if cli.mode.parallel {
            chain::ChainMode::Parallel
        } else {
            chain::ChainMode::Sequential
        };
        let mut positionals: Vec<String> = Vec::new();
        if let Some(t) = cli.task {
            positionals.push(t);
        }
        positionals.extend(cli.args);
        let items = chain::parse::parse_task_list(&positionals)?;
        let c = chain::Chain {
            mode,
            items,
            failure: overrides.failure_policy,
        };
        return chain::exec::run_chain(&ctx, &overrides, &c);
    }
    match cli.task {
        None => {
            cmd::info(&ctx, &overrides, false, schema_version)?;
            Ok(0)
        }
        Some(task) => cmd::run(&ctx, &overrides, &task, &cli.args, None),
    }
}

/// Extracts the filename portion from an argv[0]-style `OsString`, returning it when non-empty.
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

fn resolve_project_dir(project_dir: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let dir = match project_dir {
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
    cmd::run(ctx, overrides, task, &args, None)
}

/// Resolve the effective JSON schema version for this invocation: explicit
/// `--schema-version=N` wins, otherwise default to the latest version this
/// binary produces. Validates the range so `--schema-version=99` errors
/// the same on `doctor` (json) and `info` (human) — version negotiation
/// is uniform across the four JSON surfaces and their human siblings.
fn resolve_schema_version(requested: Option<u32>) -> Result<u32> {
    schema::validate_schema_version(requested.unwrap_or(schema::CURRENT_VERSION))
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
            explain: cli.global.explain,
        },
        cli::ChainFailureFlags {
            keep_going: cli_keep_going,
            kill_on_fail: cli_kill_on_fail,
        },
        loaded_config,
    )
}

fn dispatch(cli: cli::Cli, dir: &Path) -> Result<i32> {
    let ctx = detect::detect(dir);
    let loaded_config = config::load(dir)?;
    let overrides = build_overrides(&cli, loaded_config.as_ref())?;
    let schema_version = resolve_schema_version(cli.global.schema_version)?;

    match cli.command {
        Some(cli::Command::Info { json: false }) if has_task(&ctx, "info") => {
            cmd::run(&ctx, &overrides, "info", &[], None)
        }
        None => {
            cmd::info(&ctx, &overrides, false, schema_version)?;
            Ok(0)
        }
        Some(cli::Command::Info { json }) => {
            cmd::info(&ctx, &overrides, json, schema_version)?;
            Ok(0)
        }
        Some(cli::Command::Run {
            task, args, mode, ..
        }) => dispatch_run(&ctx, &overrides, task, args, mode),
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                cmd::info(&ctx, &overrides, false, schema_version)?;
                Ok(0)
            } else {
                cmd::run(&ctx, &overrides, &args[0], &args[1..], None)
            }
        }
        Some(cli::Command::Install {
            frozen: false,
            tasks,
            ..
        }) if tasks.is_empty() && has_task(&ctx, "install") => {
            cmd::run(&ctx, &overrides, "install", &[], None)
        }
        Some(cli::Command::Install { frozen, tasks, .. }) if !tasks.is_empty() => {
            dispatch_install_chain(&ctx, &overrides, frozen, &tasks)
        }
        Some(cli::Command::Install { frozen, .. }) => {
            cmd::install(&ctx, frozen)?;
            Ok(0)
        }
        Some(cli::Command::Clean {
            yes: false,
            include_framework: false,
        }) if has_task(&ctx, "clean") => cmd::run(&ctx, &overrides, "clean", &[], None),
        Some(cli::Command::Clean {
            yes,
            include_framework,
        }) => {
            cmd::clean(&ctx, yes, include_framework)?;
            Ok(0)
        }
        Some(cli::Command::List {
            raw: false,
            json: false,
            source: None,
        }) if has_task(&ctx, "list") => cmd::run(&ctx, &overrides, "list", &[], None),
        Some(cli::Command::List { raw, json, source }) => {
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
        Some(cli::Command::Completions {
            shell: None,
            output: None,
        }) if has_task(&ctx, "completions") => cmd::run(&ctx, &overrides, "completions", &[], None),
        Some(cli::Command::Completions { shell, output }) => {
            cmd::completions(shell, output.as_deref())?;
            Ok(0)
        }
        Some(cli::Command::Doctor { json }) => {
            cmd::doctor(&ctx, &overrides, json, schema_version)?;
            Ok(0)
        }
        Some(cli::Command::Why { task, json }) => {
            cmd::why(&ctx, &overrides, &task, json, schema_version)?;
            Ok(0)
        }
    }
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
        VERSION, bin_name_from_arg0, configured_project_dir, exit_code_for_error, has_task,
        parse_cli, parse_run_alias_cli, release_url, requests_version, resolve_project_dir,
        run_alias_in_dir, run_in_dir, version_line,
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
    fn runner_cli_still_parses_install_as_builtin_when_flag_set() {
        let cli = parse_cli(["runner", "install", "--frozen"]).expect("should parse");

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
        let cli =
            parse_cli(["runner", "install", "build", "test", "--kill-on-fail"]).expect("parses");
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
