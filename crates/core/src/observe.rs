//! Look for every provider's signals in a tree. Read only.

use std::io;
use std::path::{Path, PathBuf};

use crate::evidence::{Evidence, Weight};
use crate::probe::Prober;
use crate::registry::{Provider, Registry};
use crate::scope::Scope;
use crate::signal::{Declared, Signal, SignalId};
use crate::tree::Tree;

/// Every signal of every provider found in `tree`, root first, then each
/// member, then whatever the `after_observe` hooks derive.
/// # Errors
///
/// Returns the provider, scope and underlying error when a read-only query fails.
pub fn observe(tree: &Tree, registry: &Registry) -> io::Result<Vec<Evidence>> {
    let prober = Prober::new();
    let mut found = Vec::new();
    let scopes = std::iter::once((Scope::Root, tree.root.as_path())).chain(
        tree.members.iter().filter_map(|scope| match scope {
            Scope::Member { dir, .. } => Some((scope.clone(), dir.as_path())),
            Scope::Root => None,
        }),
    );
    for (scope, dir) in scopes {
        for provider in registry.iter() {
            for (index, signal) in provider.signals.iter().enumerate() {
                found.extend(
                    look(
                        provider,
                        SignalId(index),
                        signal,
                        &scope,
                        dir,
                        tree,
                        &prober,
                    )
                    .map_err(|error| {
                        io::Error::new(
                            error.kind(),
                            format!(
                                "{} observation in {} failed: {error}",
                                provider.label,
                                dir.display()
                            ),
                        )
                    })?,
                );
            }
        }
    }
    derive(tree, registry, &mut found)?;
    Ok(found)
}

/// Add provider-derived evidence, preserving errors from every hook.
///
/// # Errors
/// Returns the provider and the hook's contextual observation error.
pub fn derive(tree: &Tree, registry: &Registry, found: &mut Vec<Evidence>) -> io::Result<()> {
    for provider in registry.iter() {
        if let Some(hook) = provider.hooks.after_observe {
            let derived = hook(tree, found).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("{} observation failed: {error}", provider.label),
                )
            })?;
            found.extend(derived);
        }
    }
    Ok(())
}

fn look(
    provider: &Provider,
    id: SignalId,
    signal: &Signal,
    scope: &Scope,
    dir: &Path,
    tree: &Tree,
    prober: &Prober,
) -> io::Result<Vec<Evidence>> {
    let evidence = |at: PathBuf, weight: Weight, declared: Option<Declared>| Evidence {
        provider: Some(provider.id),
        signal: Some(id),
        at,
        scope: scope.clone(),
        weight,
        declared,
    };
    Ok(match signal {
        Signal::File(name) => file_in(dir, name)?
            .map(|at| evidence(at, Weight::Configured, None))
            .into_iter()
            .collect(),
        Signal::FileCaseless(name) => file_in_caseless(dir, name)?
            .map(|at| evidence(at, Weight::Configured, None))
            .into_iter()
            .collect(),
        Signal::Lockfile(name) => file_in(dir, name)?
            .map(|at| evidence(at, Weight::Locked, None))
            .into_iter()
            .collect(),
        Signal::FileUpwards(name) => file_upwards(dir, &tree.root, name)?
            .map(|at| {
                let mut item = evidence(at.clone(), Weight::Configured, None);
                item.scope = crate::plan::scope_at(tree, &at);
                item
            })
            .into_iter()
            .collect(),
        Signal::ManifestField { files, path, parse } => manifest_field(dir, files, path)?
            .and_then(|(at, value)| parse(&value).map(|declared| (at, declared)))
            .map(|(at, declared)| evidence(at, Weight::Declared, Some(declared)))
            .into_iter()
            .collect(),
        Signal::EnvVar(name) => {
            if *scope != Scope::Root {
                return Ok(Vec::new());
            }
            std::env::var_os(name)
                .map(|value| evidence(PathBuf::from(value), Weight::Present, None))
                .into_iter()
                .collect()
        }
        Signal::Probe(name) => {
            if *scope != Scope::Root {
                return Ok(Vec::new());
            }
            prober
                .probe(name)
                .map(|at| evidence(at, Weight::Probed, None))
                .into_iter()
                .collect()
        }
        Signal::Ask(ask) => return ask(dir),
    })
}

fn file_in(dir: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    let path = dir.join(name);
    match path.metadata() {
        Ok(metadata) => Ok(metadata.is_file().then_some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("{}: {error}", path.display()),
        )),
    }
}

fn file_in_caseless(dir: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    if let Some(exact) = file_in(dir, name)? {
        return Ok(Some(exact));
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("{}: {error}", dir.display()),
            ));
        }
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", dir.display())))?;
        let matches = entry
            .file_name()
            .to_str()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name));
        if matches && entry.file_type()?.is_file() {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found.into_iter().next())
}

fn file_upwards(dir: &Path, root: &Path, name: &str) -> io::Result<Option<PathBuf>> {
    for ancestor in dir
        .ancestors()
        .take_while(|ancestor| ancestor.starts_with(root))
    {
        if let Some(path) = file_in(ancestor, name)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn manifest_field(
    dir: &Path,
    files: &[&str],
    path: &str,
) -> io::Result<Option<(PathBuf, serde_json::Value)>> {
    let mut present = None;
    for file in files {
        if let Some(at) = file_in(dir, file)? {
            present = Some(at);
            break;
        }
    }
    let Some(at) = present else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&at)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", at.display())))?;
    let document = parse_manifest(&at, &text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {error}", at.display()),
        )
    })?;
    Ok(path
        .split('.')
        .try_fold(&document, |node, key| node.get(key))
        .cloned()
        .map(|value| (at, value)))
}

/// Read a manifest as JSON, JSON5, YAML or TOML by its extension.
fn parse_manifest(at: &Path, text: &str) -> Result<serde_json::Value, String> {
    let extension = at
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("toml") => {
            let value = toml::from_str::<toml::Value>(text).map_err(|error| error.to_string())?;
            serde_json::to_value(value).map_err(|error| error.to_string())
        }
        Some("json5") => json5::from_str(text).map_err(|error| error.to_string()),
        Some("yaml" | "yml") => {
            let documents =
                yaml_rust2::YamlLoader::load_from_str(text).map_err(|error| error.to_string())?;
            Ok(documents
                .into_iter()
                .next()
                .map_or(serde_json::Value::Null, yaml_to_json))
        }
        _ => serde_json::from_str(text).map_err(|error| error.to_string()),
    }
}

fn yaml_to_json(yaml: yaml_rust2::Yaml) -> serde_json::Value {
    use serde_json::Value;
    use yaml_rust2::Yaml;
    match yaml {
        Yaml::Real(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::String(text), Value::Number),
        Yaml::Integer(number) => Value::from(number),
        Yaml::String(text) => Value::String(text),
        Yaml::Boolean(flag) => Value::Bool(flag),
        Yaml::Array(items) => Value::Array(items.into_iter().map(yaml_to_json).collect()),
        Yaml::Hash(map) => Value::Object(
            map.into_iter()
                .filter_map(|(key, value)| {
                    let key = match key {
                        Yaml::String(text) | Yaml::Real(text) => text,
                        Yaml::Integer(number) => number.to_string(),
                        Yaml::Boolean(flag) => flag.to_string(),
                        _ => return None,
                    };
                    Some((key, yaml_to_json(value)))
                })
                .collect(),
        ),
        Yaml::Alias(_) | Yaml::Null | Yaml::BadValue => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{file_upwards, manifest_field};
    use crate::probe::tests::TempDir;

    #[test]
    fn manifest_fields_read_json_and_toml_by_dotted_path() {
        let dir = TempDir::new("observe");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "pnpm" } } }"#,
        )
        .expect("package.json");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[build-system]\nbuild-backend = \"poetry.core.masonry.api\"\n",
        )
        .expect("pyproject.toml");
        let (_, json) = manifest_field(dir.path(), &["package.json"], "devEngines.packageManager")
            .expect("read")
            .expect("field");
        assert_eq!(json["name"], "pnpm");
        let (_, toml) = manifest_field(
            dir.path(),
            &["pyproject.toml"],
            "build-system.build-backend",
        )
        .expect("read")
        .expect("field");
        assert_eq!(toml, "poetry.core.masonry.api");
        assert!(
            manifest_field(dir.path(), &["package.json"], "engines.node")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn manifest_fields_read_json5_and_yaml_and_take_the_first_file_present() {
        let dir = TempDir::new("observe-formats");
        fs::write(
            dir.path().join("package.json5"),
            "{ packageManager: 'pnpm@9.0.0', // trailing\n}",
        )
        .expect("package.json5");
        fs::write(
            dir.path().join("package.yaml"),
            "packageManager: yarn@4.0.0\ndevEngines:\n  packageManager:\n    name: yarn\n    \
             onFail: warn\n",
        )
        .expect("package.yaml");
        let files = ["package.json", "package.json5", "package.yaml"];
        let (at, value) = manifest_field(dir.path(), &files, "packageManager")
            .expect("read")
            .expect("field");
        assert!(at.ends_with("package.json5"));
        assert_eq!(value, "pnpm@9.0.0");
        let (at, value) =
            manifest_field(dir.path(), &["package.yaml"], "devEngines.packageManager")
                .expect("read")
                .expect("field");
        assert!(at.ends_with("package.yaml"));
        assert_eq!(value["name"], "yarn");
        assert_eq!(value["onFail"], "warn");
    }

    #[test]
    fn caseless_files_match_any_spelling() {
        let dir = TempDir::new("observe-caseless");
        fs::write(dir.path().join("JUSTFILE"), "").expect("JUSTFILE");
        assert_eq!(
            super::file_in_caseless(dir.path(), "justfile").unwrap(),
            Some(dir.path().join("JUSTFILE"))
        );
        assert!(
            super::file_in_caseless(dir.path(), ".justfile")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn upward_search_stops_at_the_root() {
        let dir = TempDir::new("upwards");
        let root = dir.path().join("root");
        let leaf = root.join("a").join("b");
        fs::create_dir_all(&leaf).expect("dirs");
        fs::write(dir.path().join("mise.toml"), "").expect("outside");
        assert!(file_upwards(&leaf, &root, "mise.toml").unwrap().is_none());
        fs::write(root.join("mise.toml"), "").expect("inside");
        assert_eq!(
            file_upwards(&leaf, &root, "mise.toml").unwrap(),
            Some(root.join("mise.toml"))
        );
    }
}
