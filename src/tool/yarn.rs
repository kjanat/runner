//! Yarn — Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `yarn.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

/// `yarn <task> [args...]` (yarn infers `run`).
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg(task).args(args);
    c
}

/// `yarn install [--frozen-lockfile]`
///
/// Uses `--immutable` for Yarn 2+ and falls back to the classic flag when the
/// installed major version is 1 or cannot be detected.
pub(crate) fn install_cmd(dir: &Path, frozen: bool) -> Command {
    let yarn_major = if frozen {
        detect_major_version(dir)
    } else {
        None
    };
    install_cmd_with_major(frozen, yarn_major)
}

fn install_cmd_with_major(frozen: bool, yarn_major: Option<u32>) -> Command {
    let mut c = Command::new("yarn");
    c.arg("install");
    if frozen {
        let frozen_flag = match yarn_major {
            Some(major) if major >= 2 => "--immutable",
            _ => "--frozen-lockfile",
        };
        c.arg(frozen_flag);
    }
    c
}

fn detect_major_version(dir: &Path) -> Option<u32> {
    let output = Command::new("yarn")
        .arg("--version")
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_major_version(String::from_utf8_lossy(&output.stdout).trim())
}

fn parse_major_version(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// `yarn exec <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("yarn");
    c.arg("exec").args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::{install_cmd_with_major, parse_major_version};

    #[test]
    fn frozen_install_uses_classic_flag_for_yarn_one() {
        let args: Vec<_> = install_cmd_with_major(true, Some(1))
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--frozen-lockfile"]);
    }

    #[test]
    fn frozen_install_uses_immutable_for_yarn_two_plus() {
        let args: Vec<_> = install_cmd_with_major(true, Some(4))
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--immutable"]);
    }

    #[test]
    fn frozen_install_falls_back_when_version_missing() {
        let args: Vec<_> = install_cmd_with_major(true, None)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, ["install", "--frozen-lockfile"]);
    }

    #[test]
    fn parse_major_version_reads_first_segment() {
        assert_eq!(parse_major_version("4.1.0"), Some(4));
    }
}
