//! `runner.toml` — project-level configuration.
//!
//! The file lives at the project root. The resolver reads it as step 4 of
//! the precedence chain (after CLI flags and environment variables, before
//! manifest declarations).
//!
//! Schema:
//!
//! ```toml
//! [pm]
//! node   = "pnpm"      # one of npm|pnpm|yarn|bun|deno
//! python = "uv"        # one of uv|poetry|pipenv
//!
//! [task_runner]
//! prefer = ["just", "turbo"]
//!
//! [resolution]
//! fallback     = "probe"   # probe|npm|error
//! on_mismatch  = "warn"    # warn|error|ignore
//! ```
//!
//! `#[serde(deny_unknown_fields)]` on every section surfaces typos at
//! parse time rather than as silent no-ops. Adding a new knob is two
//! changes: a field on the matching section plus a consumer in
//! `crate::resolver`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde::Deserialize;

use crate::types::{Ecosystem, PackageManager};

/// `runner.toml` filename, expected at the project root.
pub(crate) const CONFIG_FILENAME: &str = "runner.toml";

/// Parsed `runner.toml` content plus the absolute path it was loaded from.
#[derive(Debug, Clone)]
pub(crate) struct LoadedConfig {
    /// Absolute path the config was read from. Echoed back in resolver
    /// traces and the `runner doctor` output (Phase 6).
    pub path: PathBuf,
    /// Parsed config sections.
    pub config: RunnerConfig,
}

/// Top-level schema for `runner.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct RunnerConfig {
    /// `[pm]` — per-ecosystem package-manager overrides.
    #[serde(default)]
    pub pm: PmSection,
    /// `[task_runner]` — task-runner preferences.
    #[serde(default, rename = "task_runner")]
    pub task_runner: TaskRunnerSection,
    /// `[resolution]` — resolver-policy knobs.
    #[serde(default)]
    pub resolution: ResolutionSection,
}

/// `[pm]` section — per-ecosystem package manager overrides.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct PmSection {
    /// Package manager used to dispatch Node `package.json` scripts.
    /// Valid values: `npm`, `pnpm`, `yarn`, `bun`, `deno`.
    #[cfg_attr(
        feature = "schema-gen",
        schemars(extend("enum" = ["npm", "pnpm", "yarn", "bun", "deno", null]))
    )]
    pub node: Option<String>,
    /// Package manager used for Python ecosystems.
    /// Valid values: `uv`, `poetry`, `pipenv`.
    #[cfg_attr(
        feature = "schema-gen",
        schemars(extend("enum" = ["uv", "poetry", "pipenv", null]))
    )]
    pub python: Option<String>,
}

/// `[task_runner]` section — preferred ordering for ambiguous tasks.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRunnerSection {
    /// Ranked preference list. Restricts candidates to runners in the
    /// list (in listed order); a same-named task under a runner not in
    /// the list is hard-rejected. Parsed into [`crate::types::TaskRunner`]
    /// at resolver-init time so unknown labels fail fast.
    ///
    /// Valid values: `turbo`, `nx`, `make`, `just`, `task`, `mise`,
    /// `bacon`. (Not constrained in the JSON Schema — the runtime
    /// parser emits a more helpful error than a schema-validation
    /// failure would.)
    #[serde(default)]
    pub prefer: Vec<String>,
}

/// `[resolution]` section — resolver policy knobs.
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "schema-gen", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolutionSection {
    /// `probe` (default) — PATH probe in canonical order when no signals
    /// match; `npm` — legacy silent fallback; `error` — refuse to proceed.
    #[cfg_attr(
        feature = "schema-gen",
        schemars(extend("enum" = ["probe", "npm", "error", null]))
    )]
    pub fallback: Option<String>,
    /// `warn` (default), `error`, `ignore` — how to react when declaration
    /// (manifest field) disagrees with detection (lockfile).
    #[cfg_attr(
        feature = "schema-gen",
        schemars(extend("enum" = ["warn", "error", "ignore", null]))
    )]
    pub on_mismatch: Option<String>,
}

/// Load `dir/runner.toml` if it exists.
///
/// Returns `Ok(None)` when the file is absent; `Ok(Some(_))` with the
/// parsed content otherwise. Parse errors and read errors propagate up.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub(crate) fn load(dir: &Path) -> Result<Option<LoadedConfig>> {
    let path = dir.join(CONFIG_FILENAME);
    let content = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    let config: RunnerConfig =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(Some(LoadedConfig { path, config }))
}

/// Validate `[pm].node` against the set of script-dispatching PMs.
///
/// # Errors
///
/// Returns an error if `raw` does not name a known PM, or if it names a PM
/// that cannot run `package.json` scripts (e.g. `cargo`).
pub(crate) fn parse_node_pm(raw: &str) -> Result<PackageManager> {
    let pm = PackageManager::from_label(raw)
        .ok_or_else(|| anyhow!("[pm].node: unknown package manager {raw:?}"))?;
    let eco = pm.ecosystem();
    if !matches!(eco, Ecosystem::Node | Ecosystem::Deno) {
        return Err(anyhow!(
            "[pm].node: {} cannot dispatch package.json scripts (it belongs to ecosystem {:?})",
            pm.label(),
            eco,
        ));
    }
    Ok(pm)
}

/// Validate `[pm].python` against the Python ecosystem.
///
/// # Errors
///
/// Returns an error if `raw` does not name a known PM or if the named PM
/// is not part of the Python ecosystem.
pub(crate) fn parse_python_pm(raw: &str) -> Result<PackageManager> {
    let pm = PackageManager::from_label(raw)
        .ok_or_else(|| anyhow!("[pm].python: unknown package manager {raw:?}"))?;
    if pm.ecosystem() != Ecosystem::Python {
        return Err(anyhow!(
            "[pm].python: {} is not a Python package manager",
            pm.label(),
        ));
    }
    Ok(pm)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{CONFIG_FILENAME, load, parse_node_pm, parse_python_pm};
    use crate::tool::test_support::TempDir;
    use crate::types::PackageManager;

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = TempDir::new("config-absent");
        let result = load(dir.path()).expect("absent file should be Ok(None)");

        assert!(result.is_none());
    }

    #[test]
    fn load_parses_pm_section() {
        let dir = TempDir::new("config-pm");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[pm]\nnode = \"pnpm\"\npython = \"uv\"\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.path.ends_with(CONFIG_FILENAME));
        assert_eq!(loaded.config.pm.node.as_deref(), Some("pnpm"));
        assert_eq!(loaded.config.pm.python.as_deref(), Some("uv"));
    }

    #[test]
    fn load_rejects_unknown_top_level_key() {
        let dir = TempDir::new("config-unknown-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[zoot]\nfoo = 1\n")
            .expect("config should be written");

        let err = load(dir.path()).expect_err("unknown key should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("failed to parse"));
        assert!(msg.contains("zoot") || msg.contains("unknown"));
    }

    #[test]
    fn load_rejects_unknown_pm_key() {
        let dir = TempDir::new("config-unknown-pm-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nrust = \"cargo\"\n")
            .expect("config should be written");

        let err = load(dir.path()).expect_err("unknown [pm] key should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("failed to parse"));
    }

    #[test]
    fn parse_node_pm_accepts_node_and_deno() {
        assert_eq!(parse_node_pm("pnpm").unwrap(), PackageManager::Pnpm);
        assert_eq!(parse_node_pm("bun").unwrap(), PackageManager::Bun);
        assert_eq!(parse_node_pm("deno").unwrap(), PackageManager::Deno);
    }

    #[test]
    fn parse_node_pm_rejects_cross_ecosystem() {
        let err = parse_node_pm("cargo").expect_err("cargo should not be a Node PM");
        assert!(format!("{err}").contains("cannot dispatch package.json scripts"));
    }

    #[test]
    fn parse_python_pm_accepts_uv_poetry_pipenv() {
        assert_eq!(parse_python_pm("uv").unwrap(), PackageManager::Uv);
        assert_eq!(parse_python_pm("poetry").unwrap(), PackageManager::Poetry);
        assert_eq!(parse_python_pm("pipenv").unwrap(), PackageManager::Pipenv);
    }

    #[test]
    fn parse_python_pm_rejects_node_pm() {
        let err = parse_python_pm("pnpm").expect_err("pnpm should not be Python");
        assert!(format!("{err}").contains("not a Python package manager"));
    }
}
