//! Manifest task tables.

use anyhow::Context as _;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Read a Deno task table.
pub(crate) fn deno(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
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
pub(crate) fn python(path: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: PyprojectDoc =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(doc
        .project
        .and_then(|project| project.scripts)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, target)| (name, Some(target)))
        .collect())
}

#[derive(Deserialize)]
struct Package {
    #[serde(default)]
    scripts: Option<BTreeMap<String, String>>,
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
pub(crate) fn package(path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if path.extension().is_some_and(|ext| ext == "yaml") {
        let docs = yaml_rust2::YamlLoader::load_from_str(&content)
            .with_context(|| format!("{} is not valid YAML", path.display()))?;
        let Some(root) = docs.first().and_then(yaml_rust2::Yaml::as_hash) else {
            anyhow::bail!("{} is not a YAML mapping", path.display());
        };
        return Ok(root
            .iter()
            .find_map(|(key, value)| (key.as_str() == Some("scripts")).then_some(value))
            .and_then(yaml_rust2::Yaml::as_hash)
            .into_iter()
            .flatten()
            .filter_map(|(name, body)| {
                Some((
                    name.as_str()?.to_owned(),
                    body.as_str().unwrap_or_default().to_owned(),
                ))
            })
            .collect());
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

/// The task file behind `present`: its strongest evidence named one of `names`.
fn source<'a>(present: &'a runner_core::Present, names: &[&str]) -> Option<&'a Path> {
    present
        .because
        .iter()
        .filter(|e| {
            matches!(
                e.weight,
                runner_core::Weight::Declared | runner_core::Weight::Configured
            )
        })
        .find(|e| {
            e.at.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| names.contains(&name))
        })
        .map(|e| e.at.as_path())
}

/// Package scripts in the observed scope.
///
/// # Errors
/// Returns a manifest read diagnostic.
pub fn package_tasks(
    present: &runner_core::Present,
    _: &runner_core::Tree,
) -> Result<runner_core::Extracted, runner_core::Warning> {
    let Some(path) = source(present, crate::node::MANIFESTS) else {
        return Ok(runner_core::Extracted::default());
    };
    let entries = package(path)
        .map_err(|e| runner_core::Warning::about(present.provider, format!("{e:#}")))?;
    Ok(entries
        .into_iter()
        .map(|(name, command)| {
            let mut task = super::task(present, name, None);
            task.forwards_to = super::passthrough::detect_target(&task.name, &command);
            task
        })
        .collect::<Vec<_>>()
        .into())
}

/// Python entry points in the observed scope.
///
/// # Errors
/// Returns a manifest read diagnostic.
pub fn python_tasks(
    present: &runner_core::Present,
    _: &runner_core::Tree,
) -> Result<runner_core::Extracted, runner_core::Warning> {
    let Some(path) = source(present, &["pyproject.toml"]) else {
        return Ok(runner_core::Extracted::default());
    };
    python(path)
        .map(|entries| {
            entries
                .into_iter()
                .map(|(name, description)| super::task(present, name, description))
                .collect::<Vec<_>>()
                .into()
        })
        .map_err(|e| runner_core::Warning::about(present.provider, format!("{e:#}")))
}

/// Deno tasks in the observed scope.
///
/// # Errors
/// Returns a manifest read diagnostic.
pub fn deno_tasks(
    present: &runner_core::Present,
    _: &runner_core::Tree,
) -> Result<runner_core::Extracted, runner_core::Warning> {
    let Some(path) = source(present, &["deno.json", "deno.jsonc"]) else {
        return Ok(runner_core::Extracted::default());
    };
    deno(path)
        .map(|entries| {
            entries
                .into_iter()
                .map(|(name, description)| super::task(present, name, description))
                .collect::<Vec<_>>()
                .into()
        })
        .map_err(|e| runner_core::Warning::about(present.provider, format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{deno, package, python};
    use crate::extract::test_support::TempDir;

    #[test]
    fn python_scripts_carry_their_entry_points_sorted() {
        let dir = TempDir::new("pyproject-scripts");
        let path = dir.path().join("pyproject.toml");
        fs::write(
            &path,
            "[project]\nname = \"greenpy\"\n\n[project.scripts]\ngreenpy = \
             \"greenpy.main:main\"\nbodysuit = \"greenpy.bodysuit:main\"\n",
        )
        .unwrap();
        assert_eq!(
            python(&path).unwrap(),
            [
                ("bodysuit".into(), Some("greenpy.bodysuit:main".into())),
                ("greenpy".into(), Some("greenpy.main:main".into())),
            ]
        );
    }

    #[test]
    fn a_pyproject_without_scripts_has_no_tasks() {
        let dir = TempDir::new("pyproject-no-scripts");
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, "[project]\nname = \"greenpy\"\n").unwrap();
        assert_eq!(python(&path).unwrap().len(), 0);
    }

    #[test]
    fn a_malformed_pyproject_is_an_error() {
        let dir = TempDir::new("pyproject-malformed");
        let path = dir.path().join("pyproject.toml");
        fs::write(&path, "[project.scripts").unwrap();
        let error = python(&path).unwrap_err();
        assert!(
            format!("{error:#}").contains("failed to parse"),
            "{error:#}"
        );
    }

    #[test]
    fn package_scripts_come_from_every_manifest_format() {
        for (name, body) in [
            (
                "package.json",
                r#"{ "scripts": { "build": "vite build", "test": "vitest" } }"#,
            ),
            (
                "package.json5",
                "{ scripts: { build: 'vite build', test: 'vitest' } }",
            ),
            (
                "package.yaml",
                "scripts:\n  build: vite build\n  test: vitest\n",
            ),
            (
                "package.yaml",
                "scripts: { build: vite build, test: vitest }\n",
            ),
        ] {
            let dir = TempDir::new("package-scripts");
            let path = dir.path().join(name);
            fs::write(&path, body).unwrap();
            let mut scripts = package(&path).unwrap();
            scripts.sort_unstable();
            assert_eq!(
                scripts,
                [
                    ("build".to_owned(), "vite build".to_owned()),
                    ("test".to_owned(), "vitest".to_owned()),
                ],
                "{name}: {body}"
            );
        }
    }

    #[test]
    fn a_malformed_dev_engines_field_keeps_the_scripts() {
        let dir = TempDir::new("package-devengines");
        let path = dir.path().join("package.json");
        fs::write(
            &path,
            r#"{ "devEngines": { "packageManager": "pnpm@9.0.0" }, "scripts": { "build": "vite build" } }"#,
        )
        .unwrap();
        assert_eq!(
            package(&path).unwrap(),
            [("build".into(), "vite build".into())]
        );
    }

    #[test]
    fn a_yaml_script_without_a_string_body_is_still_a_script() {
        let dir = TempDir::new("package-yaml-body");
        let path = dir.path().join("package.yaml");
        fs::write(&path, "scripts:\n  build: 1\n").unwrap();
        assert_eq!(package(&path).unwrap(), [("build".into(), String::new())]);
    }

    #[test]
    fn a_yaml_manifest_that_is_not_a_mapping_is_an_error() {
        let dir = TempDir::new("package-yaml-list");
        let path = dir.path().join("package.yaml");
        fs::write(&path, "- build\n").unwrap();
        assert!(package(&path).is_err());
    }

    #[test]
    fn deno_tasks_accept_jsonc_and_object_descriptions() {
        let dir = TempDir::new("deno-jsonc");
        let path = dir.path().join("deno.jsonc");
        fs::write(
            &path,
            r#"{
  // line comment
  "tasks": {
    "build": { "command": "vite build", "description": "Bundle for production" },
    /* block comment */
    "test": "deno test",
  },
}
"#,
        )
        .unwrap();
        assert_eq!(
            deno(&path).unwrap(),
            [
                ("build".into(), Some("Bundle for production".into())),
                ("test".into(), None),
            ]
        );
    }
}
