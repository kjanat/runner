//! `runner completions` — generate dynamic shell completion scripts.

use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap_complete::aot::Shell;
use clap_complete::env::EnvCompleter;

use crate::complete::SHELLS;

/// Write dynamic completion registration scripts for both the `runner` and
/// `run` binaries.
///
/// Resolves the target shell from the explicit argument or `$SHELL`, looks
/// up the matching completer from our [`SHELLS`] table, and calls
/// [`clap_complete::env::EnvCompleter::write_registration`] directly — once
/// per binary — so users only need a single
/// `eval "$(runner completions zsh)"` in their rc file to get completion
/// for both CLIs.
///
/// When `output` is `Some`, the scripts are written to that path (any
/// existing file is overwritten) and a confirmation line is printed to
/// stderr. Otherwise they go to stdout, byte-for-byte the same output the
/// command has always produced.
pub(crate) fn completions(shell: Option<Shell>, output: Option<&Path>) -> Result<()> {
    let shell = shell.or_else(detect_shell).context(
        "could not detect shell — set $SHELL or pass explicitly: runner completions zsh",
    )?;

    let shell_name = env_shell_name(shell);
    let completer = SHELLS
        .completer(shell_name)
        .with_context(|| format!("unsupported shell: {shell_name}"))?;

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let runner_completer = exe.to_string_lossy().into_owned();
    let run_completer = sibling_run_binary(&exe).map(|path| path.to_string_lossy().into_owned());

    if let Some(path) = output {
        let file = std::fs::File::create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut buf = BufWriter::new(file);
        write_registrations(
            completer,
            &runner_completer,
            run_completer.as_deref(),
            &mut buf,
        )?;
        buf.flush()
            .with_context(|| format!("failed to flush {}", path.display()))?;
        eprintln!("wrote completion script to {}", path.display());
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        write_registrations(
            completer,
            &runner_completer,
            run_completer.as_deref(),
            &mut handle,
        )?;
    }

    Ok(())
}

/// Emit `runner`'s registration, and — when a sibling `run` binary was
/// located — `run`'s registration separated by a blank line. Shared
/// between the stdout and file-output paths so the byte stream is
/// identical either way.
fn write_registrations(
    completer: &dyn EnvCompleter,
    runner_completer: &str,
    run_completer: Option<&str>,
    buf: &mut dyn std::io::Write,
) -> Result<()> {
    completer
        .write_registration("COMPLETE", "runner", "runner", runner_completer, buf)
        .context("failed to write runner completion script")?;

    // Only emit the `run` registration when this install actually ships the
    // alias binary next to `runner`. Emitting a bare `run` completer when
    // the sibling is missing would either hijack completion for some
    // unrelated `run` on the user's PATH or produce a broken script that
    // silently fails when invoked.
    if let Some(run_completer) = run_completer {
        buf.write_all(b"\n")
            .context("failed to write completion separator")?;
        completer
            .write_registration("COMPLETE", "run", "run", run_completer, buf)
            .context("failed to write run completion script")?;
    }

    Ok(())
}

/// Resolve the sibling `run` binary next to the `runner` executable so the
/// generated completion script can invoke it directly. Returns `None` when
/// no sibling exists — the caller skips the `run` registration in that
/// case rather than guessing at PATH resolution.
fn sibling_run_binary(runner_exe: &Path) -> Option<PathBuf> {
    let parent = runner_exe.parent()?;
    let candidate = parent.join(run_binary_filename());
    candidate.is_file().then_some(candidate)
}

const fn run_binary_filename() -> &'static str {
    if cfg!(windows) { "run.exe" } else { "run" }
}

/// Detect the current shell from `$SHELL`.
fn detect_shell() -> Option<Shell> {
    shell_from_path(Path::new(&std::env::var_os("SHELL")?))
}

/// Map a shell binary path to a [`Shell`] variant.
fn shell_from_path(path: &Path) -> Option<Shell> {
    let stem = path.file_stem()?.to_string_lossy();
    match stem.as_ref() {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        "pwsh" | "powershell" => Some(Shell::PowerShell),
        _ => None,
    }
}

const fn env_shell_name(shell: Shell) -> &'static str {
    match shell {
        Shell::Elvish => "elvish",
        Shell::Fish => "fish",
        Shell::PowerShell => "powershell",
        Shell::Zsh => "zsh",
        _ => "bash",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use clap_complete::aot::Shell;

    use super::{completions, run_binary_filename, shell_from_path};
    use crate::tool::test_support::TempDir;

    #[test]
    fn completions_writes_to_file_when_output_provided() {
        let dir = TempDir::new("runner-completions-output");
        let target = dir.path().join("runner.zsh");

        completions(Some(Shell::Zsh), Some(&target))
            .expect("completions should succeed when writing to file");

        let body = fs::read_to_string(&target).expect("script file should be readable");
        assert!(
            body.starts_with("#compdef runner"),
            "file should start with runner's compdef header, got: {}",
            body.lines().next().unwrap_or_default()
        );
        assert!(
            body.contains("_clap_dynamic_completer_runner"),
            "file should contain runner's completion function"
        );
    }

    #[test]
    fn completions_errors_when_parent_dir_missing() {
        let dir = TempDir::new("runner-completions-missing-parent");
        let bad = dir.path().join("does-not-exist").join("runner.zsh");

        let err = completions(Some(Shell::Zsh), Some(&bad))
            .expect_err("missing parent directory should fail");

        assert!(
            err.to_string().contains("failed to create"),
            "error should name the file we failed to create, got: {err}"
        );
    }

    #[test]
    fn run_binary_filename_matches_platform() {
        if cfg!(windows) {
            assert_eq!(run_binary_filename(), "run.exe");
        } else {
            assert_eq!(run_binary_filename(), "run");
        }
    }

    #[test]
    fn shell_from_path_parses_bash() {
        assert_eq!(shell_from_path(Path::new("/bin/bash")), Some(Shell::Bash));
    }

    #[test]
    fn shell_from_path_parses_zsh() {
        assert_eq!(shell_from_path(Path::new("/usr/bin/zsh")), Some(Shell::Zsh));
    }

    #[test]
    fn shell_from_path_parses_fish() {
        assert_eq!(
            shell_from_path(Path::new("/usr/local/bin/fish")),
            Some(Shell::Fish)
        );
    }

    #[test]
    fn shell_from_path_returns_none_for_unknown() {
        assert_eq!(shell_from_path(Path::new("/usr/bin/ksh")), None);
    }

    #[test]
    fn shell_from_path_handles_pwsh() {
        assert_eq!(
            shell_from_path(Path::new("/usr/bin/pwsh")),
            Some(Shell::PowerShell)
        );
    }
}
