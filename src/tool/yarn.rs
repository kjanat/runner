//! Yarn — Node.js package manager.

use std::path::Path;
use std::process::Command;

/// Detected via `yarn.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

/// `yarn <task> [args...]` (yarn infers `run`).
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("yarn");
    c.arg(task).args(args);
    c
}

/// `yarn install [--frozen-lockfile] [--ignore-scripts]`
///
/// Both the frozen and the deny-scripts mechanism are version-dependent, so
/// the installed major version is probed whenever either is requested. Yarn 2+
/// uses `--immutable` for frozen and the `YARN_ENABLE_SCRIPTS=false` env for
/// deny (Berry dropped `--ignore-scripts`); Yarn 1 / undetected falls back to
/// the classic `--frozen-lockfile` and `--ignore-scripts` flags.
pub(crate) fn install_cmd(dir: &Path, frozen: bool, deny_scripts: bool) -> Command {
    let yarn_major = if frozen || deny_scripts {
        detect_major_version(dir)
    } else {
        None
    };
    install_cmd_with_major(frozen, deny_scripts, yarn_major)
}

fn install_cmd_with_major(frozen: bool, deny_scripts: bool, yarn_major: Option<u32>) -> Command {
    let is_berry = matches!(yarn_major, Some(major) if major >= 2);
    let mut c = super::program::command("yarn");
    c.arg("install");
    if frozen {
        c.arg(if is_berry {
            "--immutable"
        } else {
            "--frozen-lockfile"
        });
    }
    if deny_scripts {
        // Berry (v2+) has no `--ignore-scripts` flag; `enableScripts` is the
        // config knob, settable per-invocation via its env var. Classic (v1)
        // and the undetected fallback take the CLI flag.
        if is_berry {
            c.env("YARN_ENABLE_SCRIPTS", "false");
        } else {
            c.arg("--ignore-scripts");
        }
    }
    c
}

fn detect_major_version(dir: &Path) -> Option<u32> {
    let output = super::program::command("yarn")
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

/// `yarn exec <args...>` (Yarn 2+) or `yarn run <args...>` (Yarn 1).
///
/// Yarn Classic (v1) does not expose an `exec` subcommand —
/// `yarn run <bin>` is the documented way to run a binary out of
/// `node_modules/.bin/` there. Yarn Berry (v2+) ships a dedicated
/// `yarn exec` subcommand for the same job. We pick the right form
/// based on the installed major version, mirroring the
/// `install_cmd` version-aware pattern.
///
/// When detection fails (no `yarn` on PATH, weird output) we default
/// to the Classic-compatible `yarn run`. Yarn Berry also accepts
/// `yarn run <bin>` for binaries that live in the project's
/// `node_modules/.bin/`, so the Classic-default behaves correctly
/// on Berry projects too — at the cost of routing through Berry's
/// script lookup rather than the dedicated exec primitive.
pub(crate) fn exec_cmd(dir: &Path, args: &[String]) -> Command {
    let yarn_major = detect_major_version(dir);
    exec_cmd_with_major(yarn_major, args)
}

fn exec_cmd_with_major(yarn_major: Option<u32>, args: &[String]) -> Command {
    let mut c = super::program::command("yarn");
    let subcommand = match yarn_major {
        Some(major) if major >= 2 => "exec",
        _ => "run",
    };
    c.arg(subcommand).args(args);
    c
}

#[cfg(test)]
mod tests {
    use super::{exec_cmd_with_major, install_cmd_with_major, parse_major_version};

    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn has_env(cmd: &std::process::Command, key: &str, value: &str) -> bool {
        cmd.get_envs()
            .any(|(k, v)| k == std::ffi::OsStr::new(key) && v == Some(std::ffi::OsStr::new(value)))
    }

    #[test]
    fn frozen_install_uses_classic_flag_for_yarn_one() {
        assert_eq!(
            args_of(&install_cmd_with_major(true, false, Some(1))),
            ["install", "--frozen-lockfile"]
        );
    }

    #[test]
    fn frozen_install_uses_immutable_for_yarn_two_plus() {
        assert_eq!(
            args_of(&install_cmd_with_major(true, false, Some(4))),
            ["install", "--immutable"]
        );
    }

    #[test]
    fn frozen_install_falls_back_when_version_missing() {
        assert_eq!(
            args_of(&install_cmd_with_major(true, false, None)),
            ["install", "--frozen-lockfile"]
        );
    }

    #[test]
    fn deny_scripts_uses_ignore_scripts_flag_on_classic() {
        // Yarn 1 keeps the CLI flag.
        assert_eq!(
            args_of(&install_cmd_with_major(false, true, Some(1))),
            ["install", "--ignore-scripts"]
        );
    }

    #[test]
    fn deny_scripts_uses_env_not_flag_on_berry() {
        // Berry (v2+) dropped `--ignore-scripts`; the deny is expressed as the
        // `YARN_ENABLE_SCRIPTS=false` env, leaving the argv at a bare install.
        let cmd = install_cmd_with_major(false, true, Some(4));
        assert!(
            has_env(&cmd, "YARN_ENABLE_SCRIPTS", "false"),
            "Berry deny must set YARN_ENABLE_SCRIPTS=false",
        );
        assert_eq!(args_of(&cmd), ["install"]);
    }

    #[test]
    fn deny_scripts_falls_back_to_flag_when_version_missing() {
        // Undetected version defaults to the Classic-compatible flag, matching
        // the frozen fallback.
        assert_eq!(
            args_of(&install_cmd_with_major(false, true, None)),
            ["install", "--ignore-scripts"]
        );
    }

    #[test]
    fn frozen_and_deny_scripts_combine_on_berry() {
        let cmd = install_cmd_with_major(true, true, Some(4));
        assert!(has_env(&cmd, "YARN_ENABLE_SCRIPTS", "false"));
        assert_eq!(args_of(&cmd), ["install", "--immutable"]);
    }

    #[test]
    fn parse_major_version_reads_first_segment() {
        assert_eq!(parse_major_version("4.1.0"), Some(4));
    }

    #[test]
    fn exec_uses_run_subcommand_on_yarn_one() {
        // Yarn Classic has no `exec` subcommand. `yarn run <bin>`
        // dispatches a binary from node_modules/.bin/ there.
        let args = [String::from("eslint"), String::from("src")];
        let built: Vec<_> = exec_cmd_with_major(Some(1), &args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "eslint", "src"]);
    }

    #[test]
    fn exec_uses_exec_subcommand_on_yarn_berry() {
        let args = [String::from("eslint"), String::from("src")];
        let built: Vec<_> = exec_cmd_with_major(Some(4), &args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["exec", "eslint", "src"]);
    }

    #[test]
    fn exec_falls_back_to_run_when_version_missing() {
        // Without a detected major version we default to Yarn
        // Classic's `run` form — works on both Classic (canonical)
        // and Berry (Berry's `yarn run <bin>` also dispatches a
        // bin from node_modules/.bin/, just not via the dedicated
        // exec primitive). Erring toward `run` is the safe choice
        // because Classic genuinely lacks `exec` and would error
        // hard, whereas Berry tolerates `run`.
        let args = [String::from("eslint")];
        let built: Vec<_> = exec_cmd_with_major(None, &args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "eslint"]);
    }
}
