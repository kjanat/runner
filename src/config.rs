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
//! Parsing is **forward-compatible**: an unknown section or field (a typo,
//! or a key a newer `runner` added) is ignored rather than fatal, so a
//! config written by one version never bricks task dispatch under another.
//! Unknown keys are still surfaced as warnings (see [`collect_unknown_keys`])
//! so genuine typos stay visible. The JSON Schema keeps
//! `additionalProperties: false` (via `schemars(deny_unknown_fields)`), so
//! editors flag typos inline even though the runtime tolerates them.
//!
//! Adding a new knob is two changes: a field on the matching section plus a
//! consumer in `crate::resolver`. Keep [`KNOWN_SCHEMA`] in sync so the new
//! key isn't mis-reported as unknown.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::types::{DetectionWarning, Ecosystem, PackageManager};

/// `runner.toml` filename, expected at the project root.
pub(crate) const CONFIG_FILENAME: &str = "runner.toml";

/// Starter `runner.toml` scaffolded by `runner config init`. Every knob is
/// present, set to its built-in default, and commented out — uncommenting a
/// line is the only edit needed to override it. Keep in sync with the
/// section structs below; `config init` is the most discoverable docs we
/// ship, so a missing knob here is effectively an undocumented feature.
pub(crate) const INIT_TEMPLATE: &str = r#"# runner.toml — project task-runner configuration.
# Docs: https://runner.kjanat.dev
#
# Every key below shows its built-in default and is commented out. Uncomment
# and edit the ones you want to pin. Precedence, highest first:
#   CLI flags  >  RUNNER_* env vars  >  this file  >  manifest declarations.

# Force the package manager per ecosystem, overriding lockfile detection.
[pm]
# node = "pnpm"          # npm | pnpm | yarn | bun | deno
# python = "uv"          # uv | poetry | pipenv

# Restrict and rank task runners for ambiguous task names. Candidates not in
# the list are rejected; earlier entries win.
[task_runner]
# prefer = ["just", "turbo"]   # turbo, nx, make, just, task, mise, bacon

# Restrict which detected package managers `runner install` runs. Empty/absent
# installs every detected PM. Overridden by RUNNER_INSTALL_PMS (comma-separated).
[install]
# pms = ["bun"]                # only install with these; each must be detected

# Resolver policy knobs.
[resolution]
# fallback = "probe"     # probe (PATH probe) | npm (legacy) | error
# on_mismatch = "warn"   # warn | error (exit 2) | ignore  (manifest vs lockfile)

# Failure policy for `-s`/`-p` task chains and `install <tasks>`.
# keep_going and kill_on_fail are mutually exclusive — setting both is an error.
[chain]
# keep_going = false     # run every task despite failures (same as -k)
# kill_on_fail = false   # parallel: kill siblings on first failure (same as -K)

# GitHub Actions output grouping. Both keys take effect only under Actions.
[github]
# group_output = true    # wrap each task's output in a collapsible ::group::
# group_parallel = true  # buffer parallel tasks, print each as one block

# Parallel (`-p`) output presentation outside GitHub Actions.
[parallel]
# grouped = false        # buffer + print each task as one block on completion
"#;

/// Parsed `runner.toml` content plus the absolute path it was loaded from.
#[derive(Debug, Clone)]
pub(crate) struct LoadedConfig {
    /// Absolute path the config was read from. Echoed back in resolver
    /// traces and the `runner doctor` output (Phase 6).
    pub path: PathBuf,
    /// Parsed config sections.
    pub config: RunnerConfig,
    /// Unknown sections/fields the parse tolerated (forward compat). Carried
    /// so the dispatcher can fold them into `ctx.warnings` and `config
    /// validate` can report them, instead of silently dropping them.
    pub warnings: Vec<DetectionWarning>,
}

/// Top-level schema for `runner.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
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
    /// `[chain]` — failure policy for multi-task chains.
    #[serde(default)]
    pub chain: ChainSection,
    /// `[github]` — GitHub Actions integration (output grouping).
    #[serde(default)]
    pub github: GitHubSection,
    /// `[parallel]` — presentation of parallel (`-p`) chain output.
    #[serde(default)]
    pub parallel: ParallelSection,
    /// `[install]` — restrict which detected PMs `runner install` runs.
    #[serde(default)]
    pub install: InstallSection,
}

/// `[install]` section — restrict which detected package managers
/// `runner install` runs with. Absent or empty installs every detected
/// PM (the default). Overridden by `RUNNER_INSTALL_PMS`.
///
/// Unlike `[pm]` (which scopes *script dispatch* per ecosystem), this
/// scopes the *install fan-out*: in a polyglot repo where both `bun` and
/// `deno` would write `node_modules`, `pms = ["bun"]` keeps install to bun.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct InstallSection {
    /// Allowlist of package-manager labels to install with, e.g.
    /// `["bun"]`. Each must be a detected PM or `runner install` errors.
    /// Empty = install with every detected PM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pms: Vec<String>,
}

/// `[chain]` section — failure policy for `run -s/-p` chains and
/// `runner install <tasks>`.
///
/// `Option<bool>` rather than `bool` so the resolver can distinguish
/// "user explicitly set false" from "user didn't say": env-overrides-
/// config layering means `[chain].keep_going = false` plus
/// `RUNNER_KEEP_GOING=1` resolves to `true`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
#[cfg_attr(
    feature = "schema",
    schemars(extend("not" = {
        "required": ["keep_going", "kill_on_fail"],
        "properties": {
            "keep_going": { "const": true },
            "kill_on_fail": { "const": true }
        }
    }))
)]
pub(crate) struct ChainSection {
    /// Run every task in the chain to completion regardless of failures.
    /// Mutually exclusive with `kill_on_fail`. Equivalent to `-k` /
    /// `RUNNER_KEEP_GOING`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_going: Option<bool>,

    /// Parallel only: terminate sibling tasks immediately on first
    /// failure (forcible kill, not graceful shutdown — uncatchable on
    /// Unix). Mutually exclusive with `keep_going`. Equivalent to
    /// `--kill-on-fail` / `RUNNER_KILL_ON_FAIL`. Ignored in sequential
    /// contexts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kill_on_fail: Option<bool>,
}

/// `[github]` section — GitHub Actions integration. Both knobs only take
/// effect under GitHub Actions (gated at the call site by
/// `actions_rs::env::is_github_actions`); in a normal terminal nothing here
/// changes behavior.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct GitHubSection {
    /// Wrap task output in `runner: <task>` groups under GitHub Actions.
    /// Defaults to `true`; set `false` to restore the old ungrouped output,
    /// including the live `[task]`-prefixed muxer for parallel runs.
    #[serde(default = "default_group_output")]
    pub group_output: bool,

    /// Under GitHub Actions, group parallel (`-p`) output: buffer each task
    /// and print it as one block on completion instead of interleaving lines
    /// live. Defaults to `true` (CI logs read better grouped), but only when
    /// [`Self::group_output`] is also true. The non-CI equivalent is
    /// `[parallel].grouped` (default `false`), so CI and local diverge unless
    /// you set them to match.
    #[serde(default = "default_github_group_parallel")]
    pub group_parallel: bool,
}

impl Default for GitHubSection {
    fn default() -> Self {
        Self {
            group_output: default_group_output(),
            group_parallel: default_github_group_parallel(),
        }
    }
}

/// Default for [`GitHubSection::group_output`]: grouping is on unless the
/// user opts out, so the CI-readability win is automatic.
const fn default_group_output() -> bool {
    true
}

/// Default for [`GitHubSection::group_parallel`]: under GitHub Actions,
/// parallel output is grouped by default for readable CI logs.
const fn default_github_group_parallel() -> bool {
    true
}

/// `[parallel]` section — how parallel (`-p`) chains present their output
/// **outside** GitHub Actions. (Under GitHub Actions, see
/// [`GitHubSection::group_parallel`].)
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct ParallelSection {
    /// Buffer each parallel task's output and print it as one contiguous
    /// block the moment that task finishes (completion order — first done,
    /// first shown), instead of interleaving prefixed lines live. Defaults to
    /// `false` (the live `[task]`-prefixed muxer); set `true` to group even in
    /// a plain terminal, where a colored header delimits each block.
    #[serde(default)]
    pub grouped: bool,
}

/// `[pm]` section — per-ecosystem package manager overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct PmSection {
    /// Package manager used to dispatch Node `package.json` scripts.
    /// Valid values: `npm`, `pnpm`, `yarn`, `bun`, `deno`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["npm", "pnpm", "yarn", "bun", "deno", null]))
    )]
    pub node: Option<String>,
    /// Package manager used for Python ecosystems.
    /// Valid values: `uv`, `poetry`, `pipenv`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["uv", "poetry", "pipenv", null]))
    )]
    pub python: Option<String>,
}

/// `[task_runner]` section — preferred ordering for ambiguous tasks.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<String>,
}

/// `[resolution]` section — resolver policy knobs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(deny_unknown_fields)
)]
pub(crate) struct ResolutionSection {
    /// `probe` (default) — PATH probe in canonical order when no signals
    /// match; `npm` — legacy silent fallback; `error` — refuse to proceed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["probe", "npm", "error", null]))
    )]
    pub fallback: Option<String>,
    /// `warn` (default), `error`, `ignore` — how to react when declaration
    /// (manifest field) disagrees with detection (lockfile).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(extend("enum" = ["warn", "error", "ignore", null]))
    )]
    pub on_mismatch: Option<String>,
}

/// Recognized sections and their fields, mirroring the section structs and
/// [`INIT_TEMPLATE`]. A key absent from this table is reported as an
/// [`DetectionWarning::UnknownConfigKey`] rather than aborting the load, so a
/// config written by a newer `runner` never bricks an older binary (and vice
/// versa). Keep in sync when adding a section or field — the
/// `known_schema_covers_every_section` test guards section-level drift.
const KNOWN_SCHEMA: &[(&str, &[&str])] = &[
    ("pm", &["node", "python"]),
    ("task_runner", &["prefer"]),
    ("install", &["pms"]),
    ("resolution", &["fallback", "on_mismatch"]),
    ("chain", &["keep_going", "kill_on_fail"]),
    ("github", &["group_output", "group_parallel"]),
    ("parallel", &["grouped"]),
];

/// Collect forward-compat warnings for sections/fields this build doesn't
/// recognize. Walks the raw parsed table against [`KNOWN_SCHEMA`]; a
/// non-table where a section is expected is left for the typed deserialize to
/// reject (a genuine type error, not version skew).
fn collect_unknown_keys(value: &toml::Value) -> Vec<DetectionWarning> {
    let Some(table) = value.as_table() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for (section, body) in table {
        let Some((_, known_fields)) = KNOWN_SCHEMA.iter().find(|(name, _)| name == section) else {
            warnings.push(DetectionWarning::UnknownConfigKey {
                path: section.clone(),
            });
            continue;
        };
        if let Some(body) = body.as_table() {
            for field in body.keys() {
                if !known_fields.contains(&field.as_str()) {
                    warnings.push(DetectionWarning::UnknownConfigKey {
                        path: format!("{section}.{field}"),
                    });
                }
            }
        }
    }
    warnings
}

/// Load `dir/runner.toml` if it exists.
///
/// Returns `Ok(None)` when the file is absent; `Ok(Some(_))` otherwise. The
/// parse is forward-compatible: unknown sections/fields are tolerated (and
/// returned as `warnings`) so version skew never aborts the load. Genuine
/// failures — unreadable file, malformed TOML, or a wrong-typed *known* field
/// — still propagate as errors.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read, isn't valid TOML,
/// or assigns the wrong type to a recognized field.
pub(crate) fn load(dir: &Path) -> Result<Option<LoadedConfig>> {
    let path = dir.join(CONFIG_FILENAME);
    let content = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    // Parse once into a generic value: it lets us surface unknown keys as
    // warnings (forward compat) while still letting a wrong-typed known field
    // fail the typed conversion below.
    let value: toml::Value =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    let warnings = collect_unknown_keys(&value);
    let config: RunnerConfig = value
        .try_into()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(Some(LoadedConfig {
        path,
        config,
        warnings,
    }))
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

    use super::{
        CONFIG_FILENAME, INIT_TEMPLATE, KNOWN_SCHEMA, LoadedConfig, load, parse_node_pm,
        parse_python_pm,
    };
    use crate::tool::test_support::TempDir;
    use crate::types::{DetectionWarning, PackageManager};

    /// Dotted paths of the unknown-key warnings a load produced.
    fn unknown_paths(loaded: &LoadedConfig) -> Vec<String> {
        loaded
            .warnings
            .iter()
            .filter_map(|w| match w {
                DetectionWarning::UnknownConfigKey { path } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

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
    fn load_warns_on_unknown_section_without_failing() {
        // Forward compat: a section this build doesn't know (a typo, or one a
        // newer runner added) must not abort the load — it warns and the rest
        // of the config still applies.
        let dir = TempDir::new("config-unknown-key");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[pm]\nnode = \"bun\"\n[zoot]\nfoo = 1\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown section must be tolerated, not fatal")
            .expect("config should be present");

        assert_eq!(unknown_paths(&loaded), vec!["zoot".to_string()]);
        // Known config beside the unknown section is still honored.
        assert_eq!(loaded.config.pm.node.as_deref(), Some("bun"));
    }

    #[test]
    fn load_warns_on_unknown_field_within_known_section() {
        let dir = TempDir::new("config-unknown-pm-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nrust = \"cargo\"\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown field must be tolerated, not fatal")
            .expect("config should be present");

        assert_eq!(unknown_paths(&loaded), vec!["pm.rust".to_string()]);
    }

    #[test]
    fn load_still_rejects_wrong_type_on_known_field() {
        // Forward compat tolerates *unknown* keys, not garbage in *known*
        // ones: a wrong-typed known field is a genuine error, still fatal.
        let dir = TempDir::new("config-wrong-type");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[github]\ngroup_output = \"yes\"\n",
        )
        .expect("config should be written");

        let err = load(dir.path()).expect_err("wrong type on a known field must stay fatal");
        assert!(format!("{err:#}").contains("failed to parse"));
    }

    #[test]
    fn known_schema_covers_every_init_template_section() {
        // Guard against KNOWN_SCHEMA drifting from the scaffold: every section
        // the template emits must be recognized, or `config init` would write
        // a file that immediately warns about its own keys.
        for line in INIT_TEMPLATE.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix('[') {
                let section = rest.trim_end_matches(']');
                assert!(
                    KNOWN_SCHEMA.iter().any(|(name, _)| *name == section),
                    "INIT_TEMPLATE section [{section}] missing from KNOWN_SCHEMA"
                );
            }
        }
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

    #[test]
    fn load_parses_install_section() {
        let dir = TempDir::new("config-install");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[install]\npms = [\"bun\", \"cargo\"]\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert_eq!(loaded.config.install.pms, vec!["bun", "cargo"]);
    }

    #[test]
    fn load_warns_on_unknown_install_key() {
        let dir = TempDir::new("config-unknown-install-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[install]\nfoo = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [install] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["install.foo".to_string()]);
    }

    #[test]
    fn load_parses_chain_section() {
        let dir = TempDir::new("config-chain");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[chain]\nkeep_going = true\nkill_on_fail = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert_eq!(loaded.config.chain.keep_going, Some(true));
        assert_eq!(loaded.config.chain.kill_on_fail, Some(false));
    }

    #[test]
    fn load_warns_on_unknown_chain_key() {
        let dir = TempDir::new("config-unknown-chain-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[chain]\nfast = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [chain] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["chain.fast".to_string()]);
    }

    #[test]
    fn load_parses_github_section() {
        let dir = TempDir::new("config-github");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[github]\ngroup_output = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(!loaded.config.github.group_output);
    }

    #[test]
    fn github_group_output_defaults_true_when_key_omitted() {
        let dir = TempDir::new("config-github-default");
        fs::write(dir.path().join(CONFIG_FILENAME), "[github]\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.github.group_output);
    }

    #[test]
    fn github_group_output_defaults_true_when_section_absent() {
        let dir = TempDir::new("config-github-absent");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.github.group_output);
    }

    #[test]
    fn load_warns_on_unknown_github_key() {
        let dir = TempDir::new("config-unknown-github-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[github]\nfoo = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [github] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["github.foo".to_string()]);
    }

    #[test]
    fn load_parses_parallel_grouped() {
        let dir = TempDir::new("config-parallel-grouped");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[parallel]\ngrouped = true\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.parallel.grouped);
    }

    #[test]
    fn parallel_grouped_defaults_false_when_section_absent() {
        let dir = TempDir::new("config-parallel-default");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        // Off by default outside GitHub Actions.
        assert!(!loaded.config.parallel.grouped);
    }

    #[test]
    fn load_warns_on_unknown_parallel_key() {
        let dir = TempDir::new("config-unknown-parallel-key");
        fs::write(dir.path().join(CONFIG_FILENAME), "[parallel]\nfoo = true\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("unknown [parallel] key tolerated")
            .expect("config present");
        assert_eq!(unknown_paths(&loaded), vec!["parallel.foo".to_string()]);
    }

    #[test]
    fn load_parses_github_group_parallel() {
        let dir = TempDir::new("config-github-group-parallel");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[github]\ngroup_parallel = false\n",
        )
        .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(!loaded.config.github.group_parallel);
        // group_output is independent and still defaults true.
        assert!(loaded.config.github.group_output);
    }

    #[test]
    fn github_group_parallel_defaults_true() {
        let dir = TempDir::new("config-github-group-parallel-default");
        fs::write(dir.path().join(CONFIG_FILENAME), "[github]\n")
            .expect("config should be written");

        let loaded = load(dir.path())
            .expect("config should parse")
            .expect("config should be present");

        assert!(loaded.config.github.group_parallel);
    }
}
