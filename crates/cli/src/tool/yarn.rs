//! Yarn, Node.js package manager.

use std::path::Path;
use std::process::Command;

use runner_core::{Frozen, ScriptMechanism, ScriptRequest, ScriptSupport};
use serde::Deserialize;

/// Detected via `yarn.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

pub(crate) fn quiet_capabilities(dir: &Path) -> super::HostQuietCapabilities {
    if detect_major_version(dir) == Some(1) {
        super::HostQuietCapabilities::quiet("yarn-classic", &["--silent"])
    } else {
        super::HostQuietCapabilities::unsupported("yarn-berry", "--silent is Yarn Classic-only")
    }
}

/// `yarn <task> [args...]` (yarn infers `run`).
pub(crate) fn run_cmd(task: &str, args: &[String], verbosity: super::HostVerbosity) -> Command {
    let mut c = super::program::command("yarn");
    // `--silent` is yarn's global quiet switch (classic `-s`/`--silent`).
    // yarn has no stdout-diversion primitive, so the stream axis no-ops.
    if verbosity.silences() {
        c.arg("--silent");
    }
    c.arg(task).args(args);
    c
}

/// The frozen and script switches for an install in `dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallMechanisms {
    /// How to keep the lockfile untouched.
    pub frozen: Frozen,
    /// How to deny or allow lifecycle scripts.
    pub scripts: ScriptSupport,
    /// Variables set in addition to whatever `scripts` renders.
    pub env: Vec<(&'static str, &'static str)>,
}

/// Which switches this yarn takes. Berry (2+) uses `--immutable` and the
/// `YARN_ENABLE_SCRIPTS` env; Classic takes `--frozen-lockfile` and
/// `--ignore-scripts` and runs scripts by default. The major version is probed
/// only when a switch is requested.
pub(crate) fn install_mechanisms(
    dir: &Path,
    frozen: bool,
    scripts: ScriptRequest,
) -> InstallMechanisms {
    let yarn_major = if frozen || scripts != ScriptRequest::Default {
        detect_major_version(dir)
    } else {
        None
    };
    mechanisms_for_major(scripts, yarn_major)
}

fn mechanisms_for_major(scripts: ScriptRequest, yarn_major: Option<u32>) -> InstallMechanisms {
    let is_berry = matches!(yarn_major, Some(major) if major >= 2);
    let frozen = if is_berry {
        Frozen::Flag("--immutable")
    } else {
        Frozen::Flag("--frozen-lockfile")
    };
    let mut env = Vec::new();
    let support = match yarn_major {
        Some(major) if major >= 2 => ScriptSupport {
            deny: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "false"),
            allow: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "true"),
        },
        Some(_) => ScriptSupport {
            deny: ScriptMechanism::Flag("--ignore-scripts"),
            allow: ScriptMechanism::Default,
        },
        // Undetected: the flag denies on Classic and the env denies on Berry,
        // so a misdetected version cannot fail open.
        None => {
            if scripts == ScriptRequest::Deny {
                env.push(("YARN_ENABLE_SCRIPTS", "false"));
            }
            ScriptSupport {
                deny: ScriptMechanism::Flag("--ignore-scripts"),
                allow: ScriptMechanism::Default,
            }
        }
    };
    InstallMechanisms {
        frozen,
        scripts: support,
        env,
    }
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

/// One line of `yarn bin --json`: a binary the workspace can run and the
/// package that provides it.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct AccessibleBin {
    pub name: String,
    /// The providing package's ident, `name` or `@scope/name`.
    pub source: String,
    pub path: String,
}

/// Whether `dir` is a Plug'n'Play install, which keeps dependencies out of
/// `node_modules` and resolves them through the generated loader.
pub(crate) fn is_pnp(dir: &Path) -> bool {
    dir.join(".pnp.cjs").is_file() || dir.join(".pnp.js").is_file()
}

/// `yarn bin --json` in `dir`: every binary the workspace can run and its
/// providing package. Yarn 2+ only; `None` when yarn is missing or refuses.
pub(crate) fn accessible_bins(dir: &Path) -> Option<Vec<AccessibleBin>> {
    let output = super::program::command("yarn")
        .arg("bin")
        .arg("--json")
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_accessible_bins(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Parse the NDJSON stream of `yarn bin --json`, skipping lines that are
/// not binaries (yarn's own info and warning records).
fn parse_accessible_bins(stdout: &str) -> Vec<AccessibleBin> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// `yarn dlx --package <package> <bin> [args...]`, Yarn 2+ only; classic
/// Yarn has no package-selecting exec.
pub(crate) fn exec_package_cmd(
    dir: &Path,
    package: &str,
    bin: &str,
    args: &[String],
) -> Option<Command> {
    exec_package_cmd_with_major(detect_major_version(dir), package, bin, args)
}

fn exec_package_cmd_with_major(
    yarn_major: Option<u32>,
    package: &str,
    bin: &str,
    args: &[String],
) -> Option<Command> {
    if yarn_major.is_none_or(|major| major < 2) {
        return None;
    }
    let mut c = super::program::command("yarn");
    c.arg("dlx")
        .arg("--package")
        .arg(package)
        .arg(bin)
        .args(args);
    Some(c)
}

#[cfg(test)]
mod tests {
    use runner_core::{Frozen, ScriptMechanism, ScriptRequest, ScriptSupport};

    use super::{mechanisms_for_major, parse_major_version};

    #[test]
    fn classic_and_undetected_freeze_with_frozen_lockfile() {
        assert_eq!(
            mechanisms_for_major(ScriptRequest::Default, Some(1)).frozen,
            Frozen::Flag("--frozen-lockfile")
        );
        assert_eq!(
            mechanisms_for_major(ScriptRequest::Default, None).frozen,
            Frozen::Flag("--frozen-lockfile")
        );
    }

    #[test]
    fn berry_freezes_with_immutable() {
        assert_eq!(
            mechanisms_for_major(ScriptRequest::Default, Some(4)).frozen,
            Frozen::Flag("--immutable")
        );
    }

    #[test]
    fn classic_takes_the_ignore_scripts_flag_and_runs_scripts_by_default() {
        let classic = mechanisms_for_major(ScriptRequest::Deny, Some(1));
        assert_eq!(
            classic.scripts,
            ScriptSupport {
                deny: ScriptMechanism::Flag("--ignore-scripts"),
                allow: ScriptMechanism::Default,
            }
        );
        assert!(classic.env.is_empty());
    }

    #[test]
    fn berry_toggles_scripts_through_the_env() {
        let berry = mechanisms_for_major(ScriptRequest::Deny, Some(4));
        assert_eq!(
            berry.scripts,
            ScriptSupport {
                deny: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "false"),
                allow: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "true"),
            }
        );
        assert!(berry.env.is_empty());
    }

    #[test]
    fn an_undetected_deny_covers_both_mechanisms() {
        let unknown = mechanisms_for_major(ScriptRequest::Deny, None);
        assert_eq!(
            unknown.scripts.deny,
            ScriptMechanism::Flag("--ignore-scripts")
        );
        assert_eq!(unknown.env, vec![("YARN_ENABLE_SCRIPTS", "false")]);
        assert!(
            mechanisms_for_major(ScriptRequest::Allow, None)
                .env
                .is_empty()
        );
    }

    #[test]
    fn parse_major_version_reads_first_segment() {
        assert_eq!(parse_major_version("4.1.0"), Some(4));
    }
}

#[cfg(test)]
mod verbosity_tests {
    use super::run_cmd;
    use crate::tool::{HostDiagnostics, HostVerbosity};

    fn argv(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_cmd_default_adds_no_verbosity_flag() {
        let v = HostVerbosity::default();
        assert_eq!(argv(&run_cmd("build", &[], v)), ["build"]);
    }

    #[test]
    fn accessible_bins_parse_the_ndjson_stream_and_skip_other_records() {
        use super::{AccessibleBin, parse_accessible_bins};
        let stdout = concat!(
            r#"{"name":"tsc","source":"typescript","path":"/repo/.yarn/cache/typescript.zip/bin/tsc"}"#,
            "\n",
            r#"{"type":"info","name":0,"displayName":"YN0000","indent":"","data":"tsc"}"#,
            "\n",
            r#"{"name":"lint","source":"@scope/tool","path":"/repo/.yarn/cache/tool.zip/lint.js"}"#,
            "\n",
        );
        assert_eq!(
            parse_accessible_bins(stdout),
            [
                AccessibleBin {
                    name: "tsc".to_string(),
                    source: "typescript".to_string(),
                    path: "/repo/.yarn/cache/typescript.zip/bin/tsc".to_string(),
                },
                AccessibleBin {
                    name: "lint".to_string(),
                    source: "@scope/tool".to_string(),
                    path: "/repo/.yarn/cache/tool.zip/lint.js".to_string(),
                },
            ]
        );
    }

    #[test]
    fn run_cmd_quiet_maps_to_host_flag() {
        let v = HostVerbosity {
            diagnostics: HostDiagnostics::Quiet,
            ..HostVerbosity::default()
        };
        assert_eq!(argv(&run_cmd("build", &[], v)), ["--silent", "build"]);
    }
}
