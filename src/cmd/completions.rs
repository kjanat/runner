//! `runner completions` — generate dynamic shell completion scripts.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use clap_complete::aot::Shell;

/// Write the dynamic completion registration script to stdout.
///
/// Resolves the target shell from the explicit argument or `$SHELL`, then
/// spawns the current binary with `COMPLETE=<shell>` set, which activates
/// [`clap_complete::CompleteEnv`] and emits the registration script. This
/// avoids `unsafe` env-var mutation in our own process.
pub(crate) fn completions(shell: Option<Shell>) -> Result<()> {
    let shell = shell.or_else(detect_shell).context(
        "could not detect shell — set $SHELL or pass explicitly: runner completions zsh",
    )?;

    let bin_name = completion_bin_name(std::env::args_os().next());
    let shell_name = env_shell_name(shell);

    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.env("COMPLETE", shell_name);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.arg0(&bin_name);
    }

    let output = cmd
        .output()
        .context("failed to spawn completion subprocess")?;

    if !output.status.success() && output.stdout.is_empty() {
        bail!("completion subprocess failed");
    }

    std::io::stdout()
        .write_all(&output.stdout)
        .context("failed to write completion script")?;

    Ok(())
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

fn completion_bin_name(arg0: Option<OsString>) -> String {
    arg0.and_then(|raw| {
        Path::new(&raw)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    })
    .filter(|name| !name.is_empty())
    .unwrap_or_else(|| "runner".to_string())
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
    use std::ffi::OsString;
    use std::path::Path;

    use clap_complete::aot::Shell;

    use super::{completion_bin_name, shell_from_path};

    #[test]
    fn completion_bin_name_uses_file_name_from_path() {
        assert_eq!(completion_bin_name(Some(OsString::from("/tmp/run"))), "run");
    }

    #[test]
    fn completion_bin_name_falls_back_to_runner_when_missing() {
        assert_eq!(completion_bin_name(None), "runner");
    }

    #[test]
    fn completion_bin_name_falls_back_to_runner_when_empty() {
        assert_eq!(completion_bin_name(Some(OsString::from(""))), "runner");
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
