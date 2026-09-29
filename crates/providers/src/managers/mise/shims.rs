//! Read-only mise shim diagnostics, using the inspected directory's selection.

use std::path::{Path, PathBuf};
use std::process::Command;

use runner_core::Shim;

pub(super) fn dirs() -> Vec<PathBuf> {
    let Some(mise) = runner_core::probe_with("mise", &[]) else {
        return Vec::new();
    };
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let data = std::env::var_os("MISE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(|dir| PathBuf::from(dir).join("mise")))
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("mise"))
            } else {
                home.as_ref().map(|dir| dir.join(".local/share/mise"))
            }
        });
    let user = setting(&mise, "shims_dir")
        .or_else(|| std::env::var_os("MISE_SHIMS_DIR").map(PathBuf::from))
        .or_else(|| data.map(|dir| dir.join("shims")));
    let system = setting(&mise, "system_shims_dir")
        .or_else(|| {
            std::env::var_os("MISE_SYSTEM_DATA_DIR").map(|dir| PathBuf::from(dir).join("shims"))
        })
        .or_else(|| cfg!(unix).then(|| PathBuf::from("/usr/local/share/mise/shims")));
    let mut dirs: Vec<_> = user
        .into_iter()
        .chain(system)
        .map(|dir| expand_home(dir, home.as_deref()))
        .map(|dir| dir.canonicalize().unwrap_or(dir))
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

fn expand_home(path: PathBuf, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

/// Query one path setting, never dump the user's configuration or environment.
fn setting(mise: &Path, name: &str) -> Option<PathBuf> {
    let output = Command::new(mise)
        .args(["settings", "get", name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?.trim();
    let path = PathBuf::from(text);
    path.is_absolute().then_some(path)
}

pub(super) fn resolve(tool: &str, dir: &Path) -> Shim {
    let Some(mise) = runner_core::probe_with("mise", &[]) else {
        return Shim::Unknown;
    };
    match Command::new(mise)
        .args(["which", tool])
        .current_dir(dir)
        .output()
    {
        Ok(output) => classify(
            tool,
            output.status.success(),
            &output.stdout,
            &output.stderr,
        ),
        Err(_) => Shim::Unknown,
    }
}

fn classify(tool: &str, success: bool, stdout: &[u8], stderr: &[u8]) -> Shim {
    if !success {
        let error = String::from_utf8_lossy(stderr);
        return if [
            format!("{tool} is a mise bin however it is not currently active"),
            format!("{tool} is not a mise bin."),
        ]
        .iter()
        .any(|message| error.contains(message))
        {
            Shim::NotProvisioned
        } else {
            Shim::Unknown
        };
    }
    let Ok(text) = std::str::from_utf8(stdout) else {
        return Shim::Unknown;
    };
    let path = PathBuf::from(text.trim());
    if path.is_absolute() {
        Shim::Resolved(path)
    } else {
        Shim::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_a_leading_home_component() {
        let home = std::env::temp_dir().join("mise-home");
        assert_eq!(
            expand_home(PathBuf::from("~/.custom/shims"), Some(&home)),
            home.join(".custom/shims")
        );
        assert_eq!(expand_home(PathBuf::from("~"), Some(&home)), home);
        assert_eq!(
            expand_home(PathBuf::from("~other/shims"), Some(&home)),
            PathBuf::from("~other/shims")
        );
    }

    #[test]
    fn configuration_errors_are_not_missing_tools() {
        for message in [
            "definitely-not-found: not found",
            "Variable env.MISSING not found in context",
            "tool version not installed in template",
            "no version for template variable",
            "npm is not a mise bin. Perhaps you need to install it first.",
        ] {
            assert_eq!(
                classify("bun", false, b"", message.as_bytes()),
                Shim::Unknown
            );
        }
    }

    #[test]
    fn distinguishes_inactive_tools_from_query_failures() {
        assert_eq!(
            classify(
                "bun",
                false,
                b"",
                b"mise ERROR bun is a mise bin however it is not currently active"
            ),
            Shim::NotProvisioned
        );
        assert_eq!(
            classify(
                "npm",
                false,
                b"",
                b"mise ERROR npm is not a mise bin. Perhaps you need to install it first."
            ),
            Shim::NotProvisioned
        );
        assert_eq!(
            classify("bun", false, b"", b"mise ERROR config is not trusted"),
            Shim::Unknown
        );
        assert_eq!(classify("bun", true, b"\n", b""), Shim::Unknown);
        let path = std::env::temp_dir().join("mise-installed-bun");
        assert_eq!(
            classify("bun", true, format!("{}\n", path.display()).as_bytes(), b""),
            Shim::Resolved(path)
        );
    }
}
