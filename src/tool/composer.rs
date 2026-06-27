//! Composer — the PHP dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `composer.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("composer.json").exists()
}

/// `composer install [--no-scripts]`
///
/// `--no-scripts` (appended when `deny_scripts`) skips the root scripts
/// defined in `composer.json`. Plugin code execution is gated separately by
/// the `allow-plugins` config (Composer 2.2+), which runner does not touch.
pub(crate) fn install_cmd(deny_scripts: bool) -> Command {
    let mut c = super::program::command("composer");
    c.arg("install");
    if deny_scripts {
        c.arg("--no-scripts");
    }
    c
}

#[cfg(test)]
mod tests {
    use super::install_cmd;

    #[test]
    fn plain_install_runs_scripts() {
        let args: Vec<_> = install_cmd(false)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install"]);
    }

    #[test]
    fn deny_scripts_appends_no_scripts() {
        let args: Vec<_> = install_cmd(true)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--no-scripts"]);
    }
}
