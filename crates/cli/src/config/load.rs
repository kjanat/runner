//! `runner.toml`, project-level configuration.
//!
//! ```toml
//! download = "ask"
//!
//! [runtime]
//! javascript = "node"
//!
//! [chain]
//! on_fail = "wait"
//!
//! [install]
//! frozen = true
//! scripts = false
//! tools = true
//!
//! [output]
//! warnings = true
//! timing = true
//!
//! [output.task]
//! stderr = true
//!
//! [env]
//! APP_ENV = "development"
//!
//! [tools.mise.env]
//! MISE_YES = "1"
//!
//! [tasks.build]
//! source = "package.json"
//! pm = "pnpm"
//!
//! [tasks.build.runtime]
//! javascript = "bun"
//!
//! [tasks.build.output]
//! timing = false
//! ```
//!
//! A task's `runtime`, `env` and `output` tables have the project's shapes, and
//! each leaf a task sets overrides the project's value for that task only.
//!
//! Parsing is forward-compatible: an unknown key is ignored and reported as a
//! warning (see [`collect_unknown_keys`]), while a known key of the wrong type
//! fails the load.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::chain::FailurePolicy;
use crate::provider::Named;
use crate::types::DetectionWarning;

pub(crate) use super::values::Download;

/// Canonical config filename, written by `runner config init`. Its dotfile form
/// (`.` + this) is the hidden variant; both are accepted during discovery.
pub(crate) const CONFIG_FILENAME: &str = "runner.toml";

/// Directories searched for a config, relative to the loaded directory, highest
/// precedence first: the directory itself (`""`) and its `.config/` subdir.
pub(crate) const CONFIG_DIRS: [&str; 2] = ["", ".config"];

/// Parsed `runner.toml` content plus the absolute path it was loaded from.
#[derive(Debug, Clone)]
pub(crate) struct LoadedConfig {
    /// The file the config was read from.
    pub path: PathBuf,
    /// Parsed config.
    pub config: RunnerConfig,
    /// Unknown keys the parse tolerated.
    pub warnings: Vec<DetectionWarning>,
}

/// `runner.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(
    deny_unknown_fields,
    extend("$id" = crate::schema::config_schema_url())
)]
pub(crate) struct RunnerConfig {
    /// Whether runner may download a package to run a command: `true`,
    /// `false`, or `"ask"` to confirm each download. Unset, runner asks on an
    /// interactive terminal and downloads otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<Download>,
    /// The runtime each language's scripts and files run on.
    #[serde(default, skip_serializing_if = "RuntimeSettings::is_empty")]
    pub runtime: RuntimeSettings,
    /// How a chain of tasks reacts to a failure.
    #[serde(default, skip_serializing_if = "ChainSettings::is_empty")]
    pub chain: ChainSettings,
    /// What `runner install` does.
    #[serde(default, skip_serializing_if = "InstallSettings::is_empty")]
    pub install: InstallSettings,
    /// What runner, the tools it runs and the tasks print.
    #[serde(default, skip_serializing_if = "OutputSettings::is_empty")]
    pub output: OutputSettings,
    /// Variables every process runner spawns gets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Settings for one tool, keyed by its name (`mise`, `just`, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolSettings>,
    /// Settings for one task, keyed by its name. `source:name`, `member:name`
    /// and `member:source#name` address one task among same-named ones.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: BTreeMap<String, TaskSettings>,
}

/// `[runtime]`, shared by the project and each task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RuntimeSettings {
    /// The runtime JavaScript and TypeScript run on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = js_runtime_labels()))]
    pub javascript: Option<String>,
}

impl RuntimeSettings {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `[chain]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ChainSettings {
    /// What happens after a task in a chain fails: `continue` starts the rest,
    /// `wait` starts no more and lets running tasks finish, `kill` also stops
    /// the running ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "wait"))]
    pub on_fail: Option<FailurePolicy>,
}

impl ChainSettings {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `[install]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct InstallSettings {
    /// Install exactly what the lockfile pins, without changing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = false))]
    pub frozen: Option<bool>,
    /// Run dependencies' lifecycle scripts. Unset leaves each package
    /// manager's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<bool>,
    /// Install the toolchains detected tool managers (`mise`) declare before
    /// the dependencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub tools: Option<bool>,
}

impl InstallSettings {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `[output]`: the task-level output settings plus the ones that apply to a
/// whole invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct OutputSettings {
    /// Print warnings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub warnings: Option<bool>,
    /// Print runner's error messages. A failed command still fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub errors: Option<bool>,
    /// Print a summary after a chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub summary: Option<bool>,
    /// Settings a task may override.
    #[serde(flatten)]
    pub task: TaskOutput,
    /// Parallel chains.
    #[serde(default, skip_serializing_if = "ParallelOutput::is_empty")]
    pub parallel: ParallelOutput,
}

impl OutputSettings {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `[output.parallel]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ParallelOutput {
    /// Hold each parallel task's output and print it as one block when the
    /// task ends. Unset, runner buffers under GitHub Actions and interleaves
    /// prefixed lines elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer: Option<bool>,
}

impl ParallelOutput {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The output settings `[output]` and `[tasks.<name>.output]` share.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct TaskOutput {
    /// Print the line naming the command a task runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub progress: Option<bool>,
    /// Wrap a task's output in a collapsible GitHub Actions group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub groups: Option<bool>,
    /// Print how long a task in a chain took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub timing: Option<bool>,
    /// The tool that runs a task.
    #[serde(default, skip_serializing_if = "ToolOutput::is_empty")]
    pub tool: ToolOutput,
    /// The task's own output.
    #[serde(default, skip_serializing_if = "StreamOutput::is_empty")]
    pub task: StreamOutput,
}

/// `[output.tool]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ToolOutput {
    /// Pass the tool its own quiet flag (`npm --silent`, `make -s`), where it
    /// has one that keeps the task's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = false))]
    pub quiet: Option<bool>,
}

impl ToolOutput {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `[output.task]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct StreamOutput {
    /// Show the task's stdout. `false` discards it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub stdout: Option<bool>,
    /// Show the task's stderr. `false` discards it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub stderr: Option<bool>,
}

impl StreamOutput {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// `[tools.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct ToolSettings {
    /// Variables every invocation of this tool gets, over `[env]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// `[tasks.<name>]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub(crate) struct TaskSettings {
    /// The task source that must supply this task (`just`, `package.json`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = task_source_labels()))]
    pub source: Option<String>,
    /// The package manager that runs this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("enum" = package_manager_labels()))]
    pub pm: Option<String>,
    /// The runtime this task runs on, over `[runtime]`.
    #[serde(default, skip_serializing_if = "RuntimeSettings::is_empty")]
    pub runtime: RuntimeSettings,
    /// Variables this task's process gets, over `[tools.<name>.env]` and
    /// `[env]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// This task's output, over `[output]`.
    #[serde(default, skip_serializing_if = "task_output_is_empty")]
    pub output: TaskOutput,
}

fn task_output_is_empty(output: &TaskOutput) -> bool {
    output == &TaskOutput::default()
}

fn js_runtime_labels() -> Vec<Option<&'static str>> {
    labels(crate::provider::js_runtimes())
}

fn package_manager_labels() -> Vec<Option<&'static str>> {
    labels(crate::provider::package_managers())
}

fn task_source_labels() -> Vec<Option<&'static str>> {
    labels(crate::provider::task_sources())
}

/// `ids`' labels and aliases plus `null`, the values an optional provider
/// key accepts.
fn labels(ids: Vec<runner_core::ProviderId>) -> Vec<Option<&'static str>> {
    ids.into_iter()
        .flat_map(|id| std::iter::once(id.label()).chain(id.provider().aliases.iter().copied()))
        .map(Some)
        .chain([None])
        .collect()
}

/// `RunnerConfig`'s schema, generated once per process.
pub(crate) fn schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(RunnerConfig))
            .expect("RunnerConfig schema serializes")
    })
}

/// Top-level section name to the `$defs` entry describing it.
pub(crate) fn section_def(section: &str) -> Option<&'static str> {
    schema()["properties"][section]["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/$defs/"))
}

/// The object schema `node` describes, following `$ref`s.
fn object_schema(node: &'static serde_json::Value) -> &'static serde_json::Value {
    node.get("$ref")
        .and_then(serde_json::Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| schema().pointer(pointer))
        .map_or(node, object_schema)
}

/// Warnings for every key of `value` the schema does not declare. Map values
/// are checked against the map's value schema; a wrong-typed value is left to
/// the typed parse.
pub(crate) fn collect_unknown_keys(value: &toml::Value) -> Vec<DetectionWarning> {
    let mut warnings = Vec::new();
    walk(value, schema(), &super::KeyPath::default(), &mut warnings);
    warnings
}

fn walk(
    value: &toml::Value,
    node: &'static serde_json::Value,
    path: &super::KeyPath,
    warnings: &mut Vec<DetectionWarning>,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    let node = object_schema(node);
    let properties = node
        .get("properties")
        .and_then(serde_json::Value::as_object);
    let entries = node
        .get("additionalProperties")
        .filter(|entries| entries.is_object());
    for (key, value) in table {
        let at = path.join(key.clone());
        if let Some(field) = properties.and_then(|properties| properties.get(key)) {
            walk(value, field, &at, warnings);
        } else if let Some(entry) = entries {
            walk(value, entry, &at, warnings);
        } else if properties.is_some() {
            warnings.push(DetectionWarning::UnknownConfigKey { path: at });
        }
    }
}

/// Load the project config, searching [`CONFIG_DIRS`] × plain/dotted
/// [`CONFIG_FILENAME`] in precedence order.
///
/// # Errors
///
/// Returns an error if a candidate file exists but cannot be read, isn't valid
/// TOML, or assigns the wrong type to a recognized field.
pub(crate) fn load(dir: &Path) -> Result<Option<LoadedConfig>> {
    let Some((path, content)) = read_first_candidate(dir)? else {
        return Ok(None);
    };
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

/// Read the first config file that exists, searching each [`CONFIG_DIRS`]
/// directory for the plain then dotted [`CONFIG_FILENAME`].
///
/// # Errors
///
/// Propagates any read error other than "not found".
fn read_first_candidate(dir: &Path) -> Result<Option<(PathBuf, String)>> {
    let dotted = format!(".{CONFIG_FILENAME}");
    let filenames = [CONFIG_FILENAME, dotted.as_str()];
    for subdir in CONFIG_DIRS {
        let base = if subdir.is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(subdir)
        };
        for filename in filenames {
            let path = base.join(filename);
            match fs::read_to_string(&path) {
                Ok(content) => return Ok(Some((path, content))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("failed to read {}", path.display()));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{CONFIG_FILENAME, Download, LoadedConfig, RunnerConfig, load};
    use crate::chain::FailurePolicy;
    use crate::tool::test_support::TempDir;
    use crate::types::DetectionWarning;

    fn unknown_paths(loaded: &LoadedConfig) -> Vec<String> {
        loaded
            .warnings
            .iter()
            .filter_map(|w| match w {
                DetectionWarning::UnknownConfigKey { path } => Some(path.to_string()),
                _ => None,
            })
            .collect()
    }

    fn loaded(body: &str) -> LoadedConfig {
        let dir = TempDir::new("config");
        fs::write(dir.path().join(CONFIG_FILENAME), body).expect("seed config");
        load(dir.path()).expect("parses").expect("present")
    }

    const FULL: &str = r#"
download = "ask"

[runtime]
javascript = "node"

[chain]
on_fail = "kill"

[install]
frozen = true
scripts = false
tools = true

[output]
warnings = true
errors = true
summary = false
progress = true
groups = true
timing = true

[output.parallel]
buffer = true

[output.tool]
quiet = false

[output.task]
stdout = true
stderr = false

[env]
APP_ENV = "development"

[tools.mise.env]
MISE_YES = "1"

[tasks.build]
source = "package.json"
pm = "pnpm"

[tasks.build.runtime]
javascript = "bun"

[tasks.build.env]
APP_ENV = "production"

[tasks.build.output]
timing = false

[tasks.build.output.tool]
quiet = true

[tasks.build.output.task]
stderr = true
"#;

    #[test]
    fn every_documented_key_loads_without_warnings() {
        let loaded = loaded(FULL);
        assert_eq!(unknown_paths(&loaded), Vec::<String>::new());
        let config = &loaded.config;
        assert_eq!(config.download, Some(Download::Ask));
        assert_eq!(config.runtime.javascript.as_deref(), Some("node"));
        assert_eq!(config.chain.on_fail, Some(FailurePolicy::Kill));
        assert_eq!(config.install.scripts, Some(false));
        assert_eq!(config.output.summary, Some(false));
        assert_eq!(config.output.task.timing, Some(true));
        assert_eq!(config.output.task.task.stderr, Some(false));
        assert_eq!(config.output.parallel.buffer, Some(true));
        assert_eq!(config.tools["mise"].env["MISE_YES"], "1");
        let build = &config.tasks["build"];
        assert_eq!(build.pm.as_deref(), Some("pnpm"));
        assert_eq!(build.runtime.javascript.as_deref(), Some("bun"));
        assert_eq!(build.output.timing, Some(false));
        assert_eq!(build.output.tool.quiet, Some(true));
        assert_eq!(build.output.task.stderr, Some(true));
    }

    #[test]
    fn download_takes_a_boolean_or_ask() {
        for (written, expected) in [
            ("true", Download::Allow),
            ("false", Download::Refuse),
            ("\"ask\"", Download::Ask),
        ] {
            let config: RunnerConfig =
                toml::from_str(&format!("download = {written}\n")).expect("parses");
            assert_eq!(config.download, Some(expected), "{written}");
            assert_eq!(
                toml::to_string(&config).unwrap().trim(),
                format!("download = {written}")
            );
        }
        assert!(toml::from_str::<RunnerConfig>("download = \"yes\"\n").is_err());
    }

    #[test]
    fn a_task_output_table_is_the_project_output_minus_invocation_settings() {
        let schema = super::schema();
        let keys = |def: &str| -> Vec<String> {
            let node = &schema["$defs"][def]["properties"];
            node.as_object().unwrap().keys().cloned().collect()
        };
        let task = keys("TaskOutput");
        let project = keys("OutputSettings");
        assert!(task.iter().all(|key| project.contains(key)), "{project:?}");
        let extra: Vec<&String> = project.iter().filter(|key| !task.contains(key)).collect();
        assert_eq!(extra, ["warnings", "errors", "summary", "parallel"]);
        assert_eq!(
            schema["$defs"]["TaskSettings"]["properties"]["runtime"]["$ref"],
            schema["properties"]["runtime"]["$ref"]
        );
    }

    #[test]
    fn unknown_keys_warn_at_every_depth() {
        let loaded = loaded(
            "zoot = 1\n[install]\npms = []\n[output.task]\nstdrr = true\n[tools.mise]\ninstall = \
             true\n[tasks.build]\nrunner = \"turbo\"\n[tasks.build.output.tool]\nloud = \
             true\n[env]\nANY = \"1\"\n",
        );
        assert_eq!(
            unknown_paths(&loaded),
            [
                "install.pms",
                "output.task.stdrr",
                "tasks.build.output.tool.loud",
                "tasks.build.runner",
                "tools.mise.install",
                "zoot",
            ]
        );
    }

    #[test]
    fn load_still_rejects_wrong_type_on_known_field() {
        let dir = TempDir::new("config-wrong-type");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[install]\nfrozen = \"yes\"\n",
        )
        .unwrap();
        let err = load(dir.path()).expect_err("wrong type on a known field must stay fatal");
        assert!(format!("{err:#}").contains("failed to parse"));
    }

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = TempDir::new("config-absent");
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn discovery_prefers_the_plain_file_in_the_directory_itself() {
        let dir = TempDir::new("config-precedence");
        fs::create_dir_all(dir.path().join(".config")).unwrap();
        fs::write(dir.path().join(".config/runner.toml"), "download = false\n").unwrap();
        let found = load(dir.path()).unwrap().unwrap();
        assert!(found.path.to_string_lossy().contains(".config"));
        fs::write(dir.path().join(".runner.toml"), "download = \"ask\"\n").unwrap();
        let found = load(dir.path()).unwrap().unwrap();
        assert!(found.path.ends_with(".runner.toml"));
        fs::write(dir.path().join(CONFIG_FILENAME), "download = true\n").unwrap();
        let found = load(dir.path()).unwrap().unwrap();
        assert!(found.path.ends_with(CONFIG_FILENAME));
        assert_eq!(found.config.download, Some(Download::Allow));
    }
}
