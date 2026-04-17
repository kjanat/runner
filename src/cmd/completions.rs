//! `runner completions` — generate dynamic shell completion scripts.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap_complete::aot::Shell;

use crate::complete::SHELLS;

/// Write dynamic completion registration scripts for both the `runner` and
/// `run` binaries to stdout.
///
/// Resolves the target shell from the explicit argument or `$SHELL`, looks
/// up the matching completer from our [`SHELLS`] table, and calls
/// [`clap_complete::env::EnvCompleter::write_registration`] directly — once
/// per binary — so users only need a single
/// `eval "$(runner completions zsh)"` in their rc file to get completion
/// for both CLIs.
pub(crate) fn completions(shell: Option<Shell>) -> Result<()> {
    let shell = shell.or_else(detect_shell).context(
        "could not detect shell — set $SHELL or pass explicitly: runner completions zsh",
    )?;

    let shell_name = env_shell_name(shell);
    let completer = SHELLS
        .completer(shell_name)
        .with_context(|| format!("unsupported shell: {shell_name}"))?;

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let runner_completer = exe.to_string_lossy().into_owned();
    let run_completer = sibling_run_binary(&exe).map_or_else(
        || "run".to_string(),
        |path| path.to_string_lossy().into_owned(),
    );

    let mut stdout = std::io::stdout();
    completer
        .write_registration(
            "COMPLETE",
            "runner",
            "runner",
            &runner_completer,
            &mut stdout,
        )
        .context("failed to write runner completion script")?;
    stdout
        .write_all(b"\n")
        .context("failed to write completion separator")?;
    completer
        .write_registration("COMPLETE", "run", "run", &run_completer, &mut stdout)
        .context("failed to write run completion script")?;

    Ok(())
}

/// Resolve the sibling `run` binary next to the `runner` executable so the
/// generated script can invoke it directly. Falls back to `None` (the
/// caller then uses `"run"` on `$PATH`) when no sibling exists — typical
/// for cross-compiled or split installs.
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
    use std::path::Path;

    use clap_complete::aot::Shell;

    use super::{run_binary_filename, shell_from_path};

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
