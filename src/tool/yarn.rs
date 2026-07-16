//! Yarn, Node.js package manager.

use std::path::Path;
use std::process::Command;

use super::ScriptDirective;

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
/// Both the frozen and the script-policy mechanism are version-dependent, so
/// the installed major version is probed whenever either is requested. Yarn 2+
/// (Berry) uses `--immutable` for frozen and the `YARN_ENABLE_SCRIPTS` env to
/// toggle scripts (it dropped `--ignore-scripts` and has no per-dependency
/// allowlist; `enableScripts` is a single global switch, so both deny and
/// force-on are expressible). Yarn 1 / undetected falls back to the classic
/// `--frozen-lockfile` and `--ignore-scripts` flags; Classic runs scripts by
/// default and has no `--no-ignore-scripts`, so force-on is a no-op there.
pub(crate) fn install_cmd(dir: &Path, frozen: bool, scripts: ScriptDirective) -> Command {
    let yarn_major = if frozen || scripts != ScriptDirective::Default {
        detect_major_version(dir)
    } else {
        None
    };
    install_cmd_with_major(frozen, scripts, yarn_major)
}

fn install_cmd_with_major(
    frozen: bool,
    scripts: ScriptDirective,
    yarn_major: Option<u32>,
) -> Command {
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
    match scripts {
        ScriptDirective::Deny => match yarn_major {
            // Berry (v2+) dropped `--ignore-scripts`; `enableScripts` is the
            // config knob, set per-invocation via its env var.
            Some(major) if major >= 2 => {
                c.env("YARN_ENABLE_SCRIPTS", "false");
            }
            // Classic (v1) takes the CLI flag.
            Some(_) => {
                c.arg("--ignore-scripts");
            }
            // Version undetected (a failed `yarn --version`). Deny is
            // security-sensitive, so cover both mechanisms rather than silently
            // assume Classic: the flag denies on Classic (which ignores the
            // env), and the env denies on Berry (which would otherwise reject
            // the flag, or, worse, accept-and-ignore it and run scripts). Belt
            // and suspenders so a misdetected version can never fail open.
            None => {
                c.arg("--ignore-scripts");
                c.env("YARN_ENABLE_SCRIPTS", "false");
            }
        },
        ScriptDirective::ForceOn => {
            // Berry's `enableScripts` is a global toggle (no per-dependency
            // allowlist), so forcing on is just the env set to `true`. Classic
            // runs scripts by default and has no negation flag, nothing to do.
            if is_berry {
                c.env("YARN_ENABLE_SCRIPTS", "true");
            }
        }
        ScriptDirective::Default => {}
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
/// Yarn Classic (v1) does not expose an `exec` subcommand;
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
/// on Berry projects too, at the cost of routing through Berry's
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
    use super::{
        ScriptDirective, exec_cmd_with_major, install_cmd_with_major, parse_major_version,
    };

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
            args_of(&install_cmd_with_major(
                true,
                ScriptDirective::Default,
                Some(1)
            )),
            ["install", "--frozen-lockfile"]
        );
    }

    #[test]
    fn frozen_install_uses_immutable_for_yarn_two_plus() {
        assert_eq!(
            args_of(&install_cmd_with_major(
                true,
                ScriptDirective::Default,
                Some(4)
            )),
            ["install", "--immutable"]
        );
    }

    #[test]
    fn frozen_install_falls_back_when_version_missing() {
        assert_eq!(
            args_of(&install_cmd_with_major(
                true,
                ScriptDirective::Default,
                None
            )),
            ["install", "--frozen-lockfile"]
        );
    }

    #[test]
    fn deny_scripts_uses_ignore_scripts_flag_on_classic() {
        // Yarn 1 keeps the CLI flag.
        assert_eq!(
            args_of(&install_cmd_with_major(
                false,
                ScriptDirective::Deny,
                Some(1)
            )),
            ["install", "--ignore-scripts"]
        );
    }

    #[test]
    fn deny_scripts_uses_env_not_flag_on_berry() {
        // Berry (v2+) dropped `--ignore-scripts`; the deny is expressed as the
        // `YARN_ENABLE_SCRIPTS=false` env, leaving the argv at a bare install.
        let cmd = install_cmd_with_major(false, ScriptDirective::Deny, Some(4));
        assert!(
            has_env(&cmd, "YARN_ENABLE_SCRIPTS", "false"),
            "Berry deny must set YARN_ENABLE_SCRIPTS=false",
        );
        assert_eq!(args_of(&cmd), ["install"]);
    }

    #[test]
    fn deny_scripts_covers_both_mechanisms_when_version_missing() {
        // Undetected version is security-sensitive on the deny path, so cover
        // both: the `--ignore-scripts` flag denies on Classic, and the
        // `YARN_ENABLE_SCRIPTS=false` env denies on Berry, neither silently
        // fails open if the version was misdetected.
        let cmd = install_cmd_with_major(false, ScriptDirective::Deny, None);
        assert_eq!(args_of(&cmd), ["install", "--ignore-scripts"]);
        assert!(
            has_env(&cmd, "YARN_ENABLE_SCRIPTS", "false"),
            "undetected deny must also set the Berry env so it can't fail open",
        );
    }

    #[test]
    fn force_on_sets_enable_scripts_env_on_berry() {
        // Berry's `enableScripts` is a single global toggle, so force-on is
        // expressible as `YARN_ENABLE_SCRIPTS=true` with a bare install argv.
        let cmd = install_cmd_with_major(false, ScriptDirective::ForceOn, Some(4));
        assert!(
            has_env(&cmd, "YARN_ENABLE_SCRIPTS", "true"),
            "Berry force-on must set YARN_ENABLE_SCRIPTS=true",
        );
        assert_eq!(args_of(&cmd), ["install"]);
    }

    #[test]
    fn force_on_is_noop_on_classic() {
        // Yarn 1 runs scripts by default and has no `--no-ignore-scripts`, so
        // force-on adds nothing (no flag, no env).
        let cmd = install_cmd_with_major(false, ScriptDirective::ForceOn, Some(1));
        assert!(!has_env(&cmd, "YARN_ENABLE_SCRIPTS", "true"));
        assert_eq!(args_of(&cmd), ["install"]);
    }

    #[test]
    fn frozen_and_deny_scripts_combine_on_berry() {
        let cmd = install_cmd_with_major(true, ScriptDirective::Deny, Some(4));
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
        // Classic's `run` form, works on both Classic (canonical)
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
