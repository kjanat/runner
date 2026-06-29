//! Composer — the PHP dependency manager.

use std::path::Path;
use std::process::Command;

use super::ScriptDirective;

/// Detected via `composer.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("composer.json").exists()
}

/// `composer install [--no-scripts]`
///
/// [`ScriptDirective::Deny`] appends `--no-scripts`, skipping the root scripts
/// defined in `composer.json`. Plugin code execution is gated separately by
/// the `allow-plugins` config (Composer 2.2+), which runner does not touch.
/// [`ScriptDirective::ForceOn`] adds nothing — composer runs scripts by
/// default, so force-on is satisfied by simply not passing `--no-scripts`.
pub(crate) fn install_cmd(scripts: ScriptDirective) -> Command {
    let mut c = super::program::command("composer");
    c.arg("install");
    if scripts == ScriptDirective::Deny {
        c.arg("--no-scripts");
    }
    c
}

#[cfg(test)]
mod tests {
    use super::{ScriptDirective, install_cmd};

    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plain_install_runs_scripts() {
        assert_eq!(args_of(&install_cmd(ScriptDirective::Default)), ["install"]);
    }

    #[test]
    fn deny_scripts_appends_no_scripts() {
        assert_eq!(
            args_of(&install_cmd(ScriptDirective::Deny)),
            ["install", "--no-scripts"]
        );
    }

    #[test]
    fn force_on_runs_scripts_without_flag() {
        // Composer runs scripts by default; force-on just omits `--no-scripts`.
        assert_eq!(args_of(&install_cmd(ScriptDirective::ForceOn)), ["install"]);
    }
}
