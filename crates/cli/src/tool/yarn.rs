//! Yarn, Node.js package manager.

use std::path::Path;
#[cfg(test)]
use std::process::Command;

use serde::Deserialize;

/// Detected via `yarn.lock`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("yarn.lock").exists()
}

/// `yarn <task> [args...]` (yarn infers `run`).
#[cfg(test)]
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
pub(crate) fn accessible_bins(dir: &Path) -> std::io::Result<Option<Vec<AccessibleBin>>> {
    let output = super::program::command("yarn")
        .arg("bin")
        .arg("--json")
        .current_dir(dir)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "yarn bin --json in {} failed ({}): {}",
            dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(Some(parse_accessible_bins(&String::from_utf8_lossy(
        &output.stdout,
    ))?))
}

/// Parse the NDJSON stream of `yarn bin --json`, skipping lines that are
/// not binaries (yarn's own info and warning records).
fn parse_accessible_bins(stdout: &str) -> std::io::Result<Vec<AccessibleBin>> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("yarn bin --json: {error}"),
                )
            })
        })
        .filter_map(|value| match value {
            Ok(value) if value.get("type").is_some() && value.get("source").is_none() => None,
            Ok(value) => Some(serde_json::from_value(value).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("yarn bin --json: {error}"),
                )
            })),
            Err(error) => Some(Err(error)),
        })
        .collect()
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
            parse_accessible_bins(stdout).unwrap(),
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
