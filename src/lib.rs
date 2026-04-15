//! # Runner
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
//! **Task runners:** [turbo], [nx], [make], [just], [go-task], [mise]
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
//!
//! ## Library API
//!
//! - [`run_from_env`] parses process args and dispatches in current dir.
//! - [`run_from_args`] parses explicit args and dispatches in current dir.
//! - [`run_in_dir`] parses explicit args and dispatches against a given dir.
//!
//! ## CLI Usage
//!
//! ```text
//! runner              # show detected project info
//! runner <task>       # run a task (auto-routed to the right tool)
//! run <task>          # alias binary for quicker task execution
//! runner install      # install dependencies via detected PM
//! runner clean        # remove caches and build artifacts
//! runner list         # list available tasks from all sources
//! runner exec <cmd>   # run a command through the package manager
//! ```
//!
//! Generate docs with `cargo doc --document-private-items --open`.

mod cli;
mod cmd;
mod complete;
mod detect;
mod tool;
mod types;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{CommandFactory, FromArgMatches};

const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
const VERSION: &str = env!("CARGO_PKG_VERSION");

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
        cli::Cli::command().name(bin.clone()).bin_name(bin.clone())
    })
    .shells(complete::SHELLS)
    .complete();
    run_from_args(std::env::args_os())
}

/// Parse explicit args, detect current dir, dispatch, return exit code.
///
/// `args` must include `argv\[0\]` as first item.
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
/// `args` must include `argv\[0\]` as first item.
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
            cli.project_dir.as_deref(),
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

    let mut command = cli::Cli::command();
    if let Some(bin_name) = args.first().and_then(bin_name_from_arg0) {
        command = command.name(bin_name.clone()).bin_name(bin_name);
    }

    let matches = command.try_get_matches_from(args)?;
    cli::Cli::from_arg_matches(&matches)
}

fn bin_name_from_arg0(arg0: &OsString) -> Option<String> {
    let name = Path::new(arg0)
        .file_name()
        .map(|segment| segment.to_string_lossy().into_owned())?;

    (!name.is_empty()).then_some(name)
}

fn requests_version(args: &[OsString]) -> bool {
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

fn dispatch(cli: cli::Cli, dir: &Path) -> Result<i32> {
    let ctx = detect::detect(dir);

    match cli.command {
        None | Some(cli::Command::Info) => {
            cmd::info(&ctx);
            Ok(0)
        }
        Some(cli::Command::Run { task, args }) => cmd::run(&ctx, &task, &args),
        Some(cli::Command::External(args)) => {
            if args.is_empty() {
                cmd::info(&ctx);
                Ok(0)
            } else {
                cmd::run(&ctx, &args[0], &args[1..])
            }
        }
        Some(cli::Command::Install { frozen }) => {
            cmd::install(&ctx, frozen)?;
            Ok(0)
        }
        Some(cli::Command::Clean {
            yes,
            include_framework,
        }) => {
            cmd::clean(&ctx, yes, include_framework)?;
            Ok(0)
        }
        Some(cli::Command::List { raw }) => {
            cmd::list(&ctx, raw);
            Ok(0)
        }
        Some(cli::Command::Exec { args }) => cmd::exec(&ctx, &args),
        Some(cli::Command::Completions { shell }) => {
            cmd::completions(shell)?;
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{
        bin_name_from_arg0, configured_project_dir, release_url, requests_version,
        resolve_project_dir, run_in_dir, version_line,
    };
    use crate::tool::test_support::TempDir;

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
            release_url("0.3.0"),
            "https://github.com/kjanat/runner/releases/tag/v0.3.0"
        );
    }

    #[test]
    fn version_line_wraps_bin_and_version_with_separate_links() {
        let line = version_line(&[OsString::from("runner")], true);

        assert!(line.contains(
            "\u{1b}]8;;https://github.com/kjanat/runner/\u{1b}\\runner\u{1b}]8;;\u{1b}\\"
        ));
        assert!(line.contains("\u{1b}]8;;https://github.com/kjanat/runner/releases/tag/v0.3.0\u{1b}\\0.3.0\u{1b}]8;;\u{1b}\\"));
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
}
