//! Manifest task tables.

use anyhow::Context as _;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Read a Deno task table.
pub fn deno(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let d = json5::from_str::<Partial>(&content)
        .with_context(|| format!("{} is not valid JSON/JSONC", path.display()))?;
    let mut tasks: Vec<(String, Option<String>)> = d.tasks.map_or_else(Vec::new, |t| {
        t.into_iter()
            .map(|(name, value)| {
                // String form carries no description; object form may.
                let description = value
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                (name, description)
            })
            .collect()
    });
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tasks)
}

/// Read Python console-script entry points.
pub fn python(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: PyprojectDoc =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    // `BTreeMap` iterates in sorted key order, so the returned list is
    // already alphabetized, matching the post-extraction sort that
    // `detect::detect` applies to the full task list.
    Ok(doc
        .project
        .and_then(|project| project.scripts)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, target)| (name, Some(target)))
        .collect())
}

#[derive(Deserialize)]
struct PyprojectDoc {
    project: Option<PyprojectProject>,
}

#[derive(Deserialize)]
struct PyprojectProject {
    scripts: Option<BTreeMap<String, String>>,
}

/// Script names and command text from a supported package manifest.
///
/// # Errors
/// Reports unreadable or malformed manifests.
pub fn package(path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if path.extension().is_some_and(|ext| ext == "yaml") {
        let docs = yaml_rust2::YamlLoader::load_from_str(&content)
            .with_context(|| format!("{} is not valid YAML", path.display()))?;
        let doc = docs.first().context("empty package manifest")?;
        let scripts = &doc["scripts"];
        return Ok(scripts
            .as_hash()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| {
                Some((name.as_str()?.to_owned(), value.as_str()?.to_owned()))
            })
            .collect());
    }
    #[derive(Deserialize)]
    struct Package {
        #[serde(default)]
        scripts: Option<BTreeMap<String, String>>,
    }
    let manifest: Package = if path.extension().is_some_and(|ext| ext == "json5") {
        json5::from_str(&content)
            .with_context(|| format!("{} is not valid JSON5", path.display()))?
    } else {
        serde_json::from_str(&content)
            .with_context(|| format!("{} is not valid JSON", path.display()))?
    };
    Ok(manifest.scripts.unwrap_or_default().into_iter().collect())
}

fn source(present: &runner_core::Present) -> Option<&Path> {
    present
        .because
        .iter()
        .find(|e| matches!(e.weight, runner_core::Weight::Configured))
        .map(|e| e.at.as_path())
}

/// Package scripts in the observed scope.
///
/// # Errors
/// Returns a manifest read diagnostic.
pub fn package_tasks(
    present: &runner_core::Present,
    _: &runner_core::Tree,
) -> Result<Vec<runner_core::Task>, runner_core::Warning> {
    let Some(path) = source(present) else {
        return Ok(Vec::new());
    };
    let entries =
        package(path).map_err(|e| runner_core::Warning::about(present.provider, e.to_string()))?;
    Ok(entries
        .into_iter()
        .map(|(name, command)| {
            let mut task = super::task(present, name, None);
            task.forwards_to = super::passthrough::detect_target(&task.name, &command);
            task
        })
        .collect())
}

/// Python entry points in the observed scope.
///
/// # Errors
/// Returns a manifest read diagnostic.
pub fn python_tasks(
    present: &runner_core::Present,
    _: &runner_core::Tree,
) -> Result<Vec<runner_core::Task>, runner_core::Warning> {
    let Some(path) = source(present) else {
        return Ok(Vec::new());
    };
    python(path)
        .map(|entries| {
            entries
                .into_iter()
                .map(|(name, description)| super::task(present, name, description))
                .collect()
        })
        .map_err(|e| runner_core::Warning::about(present.provider, e.to_string()))
}

/// Deno tasks in the observed scope.
///
/// # Errors
/// Returns a manifest read diagnostic.
pub fn deno_tasks(
    present: &runner_core::Present,
    _: &runner_core::Tree,
) -> Result<Vec<runner_core::Task>, runner_core::Warning> {
    let Some(path) = source(present) else {
        return Ok(Vec::new());
    };
    deno(path)
        .map(|entries| {
            entries
                .into_iter()
                .map(|(name, description)| super::task(present, name, description))
                .collect()
        })
        .map_err(|e| runner_core::Warning::about(present.provider, e.to_string()))
}
