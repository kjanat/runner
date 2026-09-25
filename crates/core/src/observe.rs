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
        Signal::ManifestField { file, path, parse } => manifest_field(dir, file, path)?
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
    file: &str,
    path: &str,
) -> io::Result<Option<(PathBuf, serde_json::Value)>> {
    let Some(at) = file_in(dir, file)? else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&at)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", at.display())))?;
    let invalid = |error: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {error}", at.display()),
        )
    };
    let document: serde_json::Value = if Path::new(file)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
    {
        let value =
            toml::from_str::<toml::Value>(&text).map_err(|error| invalid(error.to_string()))?;
        serde_json::to_value(value).map_err(|error| invalid(error.to_string()))?
    } else {
        serde_json::from_str(&text).map_err(|error| invalid(error.to_string()))?
    };
    Ok(path
        .split('.')
        .try_fold(&document, |node, key| node.get(key))
        .cloned()
        .map(|value| (at, value)))
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
        let (_, json) = manifest_field(dir.path(), "package.json", "devEngines.packageManager")
            .expect("read")
            .expect("field");
        assert_eq!(json["name"], "pnpm");
        let (_, toml) = manifest_field(dir.path(), "pyproject.toml", "build-system.build-backend")
            .expect("read")
            .expect("field");
        assert_eq!(toml, "poetry.core.masonry.api");
        assert!(
            manifest_field(dir.path(), "package.json", "engines.node")
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
