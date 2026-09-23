//! Look for every provider's signals in a tree. Read only.

use std::path::{Path, PathBuf};

use crate::evidence::{Evidence, Weight};
use crate::probe::Prober;
use crate::registry::{Provider, Registry};
use crate::scope::Scope;
use crate::signal::{Declared, Signal, SignalId};
use crate::tree::Tree;

/// Every signal of every provider found in `tree`, root first, then each
/// member, then whatever the `after_observe` hooks derive.
#[must_use]
pub fn observe(tree: &Tree, registry: &Registry) -> Vec<Evidence> {
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
                found.extend(look(
                    provider,
                    SignalId(index),
                    signal,
                    &scope,
                    dir,
                    tree,
                    &prober,
                ));
            }
        }
    }
    for provider in registry.iter() {
        if let Some(hook) = provider.hooks.after_observe {
            found.extend(hook(tree, &found));
        }
    }
    found
}

fn look(
    provider: &Provider,
    id: SignalId,
    signal: &Signal,
    scope: &Scope,
    dir: &Path,
    tree: &Tree,
    prober: &Prober,
) -> Vec<Evidence> {
    let evidence = |at: PathBuf, weight: Weight, declared: Option<Declared>| Evidence {
        provider: provider.id,
        signal: id,
        at,
        scope: scope.clone(),
        weight,
        declared,
    };
    match signal {
        Signal::File(name) => file_in(dir, name)
            .map(|at| evidence(at, Weight::Configured, None))
            .into_iter()
            .collect(),
        Signal::Lockfile(name) => file_in(dir, name)
            .map(|at| evidence(at, Weight::Locked, None))
            .into_iter()
            .collect(),
        Signal::FileUpwards(name) => file_upwards(dir, &tree.root, name)
            .map(|at| evidence(at, Weight::Configured, None))
            .into_iter()
            .collect(),
        Signal::ManifestField { file, path, parse } => manifest_field(dir, file, path)
            .and_then(|(at, value)| parse(&value).map(|declared| (at, declared)))
            .map(|(at, declared)| evidence(at, Weight::Declared, Some(declared)))
            .into_iter()
            .collect(),
        Signal::EnvVar(name) => {
            if *scope != Scope::Root {
                return Vec::new();
            }
            std::env::var_os(name)
                .map(|value| evidence(PathBuf::from(value), Weight::Present, None))
                .into_iter()
                .collect()
        }
        Signal::Probe(name) => {
            if *scope != Scope::Root {
                return Vec::new();
            }
            prober
                .probe(name)
                .map(|at| evidence(at, Weight::Probed, None))
                .into_iter()
                .collect()
        }
        Signal::Ask(ask) => ask(dir).unwrap_or_default(),
    }
}

fn file_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    path.is_file().then_some(path)
}

fn file_upwards(dir: &Path, root: &Path, name: &str) -> Option<PathBuf> {
    dir.ancestors()
        .take_while(|ancestor| ancestor.starts_with(root))
        .find_map(|ancestor| file_in(ancestor, name))
}

fn manifest_field(dir: &Path, file: &str, path: &str) -> Option<(PathBuf, serde_json::Value)> {
    let at = file_in(dir, file)?;
    let text = std::fs::read_to_string(&at).ok()?;
    let is_toml = Path::new(file)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    let document: serde_json::Value = if is_toml {
        serde_json::to_value(toml::from_str::<toml::Value>(&text).ok()?).ok()?
    } else {
        serde_json::from_str(&text).ok()?
    };
    let value = path
        .split('.')
        .try_fold(&document, |node, key| node.get(key))?
        .clone();
    Some((at, value))
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
        let (_, json) =
            manifest_field(dir.path(), "package.json", "devEngines.packageManager").expect("field");
        assert_eq!(json["name"], "pnpm");
        let (_, toml) = manifest_field(dir.path(), "pyproject.toml", "build-system.build-backend")
            .expect("field");
        assert_eq!(toml, "poetry.core.masonry.api");
        assert!(manifest_field(dir.path(), "package.json", "engines.node").is_none());
    }

    #[test]
    fn upward_search_stops_at_the_root() {
        let dir = TempDir::new("upwards");
        let root = dir.path().join("root");
        let leaf = root.join("a").join("b");
        fs::create_dir_all(&leaf).expect("dirs");
        fs::write(dir.path().join("mise.toml"), "").expect("outside");
        assert!(file_upwards(&leaf, &root, "mise.toml").is_none());
        fs::write(root.join("mise.toml"), "").expect("inside");
        assert_eq!(
            file_upwards(&leaf, &root, "mise.toml"),
            Some(root.join("mise.toml"))
        );
    }
}
