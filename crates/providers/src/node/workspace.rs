//! Workspaces declared at a Node root.

use std::path::Path;

use runner_core::{Declaration, Warning};
use serde_json::Value;
use yaml_rust2::YamlLoader;

use crate::workspace::{Manifest, members, read};

/// The workspaces `pnpm-workspace.yaml`, the package manifest's `workspaces`
/// and `lerna.json` declare at `root`, each member carrying a package
/// manifest.
///
/// # Errors
///
/// When a declaration or a member manifest exists but cannot be read.
pub fn declarations(root: &Path) -> Result<Vec<Declaration>, Warning> {
    let mut found = Vec::new();
    for (kind, globs) in [
        ("pnpm-workspace.yaml", pnpm_globs(root)?),
        ("package.json workspaces", manifest_globs(root)?),
        ("lerna.json", lerna_globs(root)?),
    ] {
        if let Some(globs) = globs {
            found.push(Declaration {
                kind,
                members: members(root, &globs, manifest_name)?,
            });
        }
    }
    Ok(found)
}

fn strings(list: &[Value]) -> Vec<String> {
    list.iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn pnpm_globs(root: &Path) -> Result<Option<Vec<String>>, Warning> {
    let Some(text) = read(&root.join("pnpm-workspace.yaml"))? else {
        return Ok(None);
    };
    let docs = YamlLoader::load_from_str(&text)
        .map_err(|err| Warning::general(format!("pnpm-workspace.yaml: {err}")))?;
    Ok(docs
        .first()
        .and_then(|doc| doc["packages"].as_vec())
        .map(|list| {
            list.iter()
                .filter_map(yaml_rust2::Yaml::as_str)
                .map(ToOwned::to_owned)
                .collect()
        }))
}

fn manifest(dir: &Path) -> Result<Option<Value>, Warning> {
    runner_core::read_manifest(dir, super::MANIFESTS)
        .map(|found| found.map(|(_, document)| document))
        .map_err(|error| Warning::general(error.to_string()))
}

fn manifest_globs(root: &Path) -> Result<Option<Vec<String>>, Warning> {
    Ok(
        manifest(root)?.and_then(|document| match &document["workspaces"] {
            Value::Array(list) => Some(strings(list)),
            Value::Object(map) => Some(
                map.get("packages")
                    .and_then(Value::as_array)
                    .map_or_else(Vec::new, |list| strings(list)),
            ),
            _ => None,
        }),
    )
}

fn lerna_globs(root: &Path) -> Result<Option<Vec<String>>, Warning> {
    let Some(text) = read(&root.join("lerna.json"))? else {
        return Ok(None);
    };
    let lerna: Value = serde_json::from_str(&text)
        .map_err(|err| Warning::general(format!("lerna.json: {err}")))?;
    if lerna["useWorkspaces"].as_bool() == Some(true) {
        return Ok(None);
    }
    Ok(Some(lerna["packages"].as_array().map_or_else(
        || vec!["packages/*".to_owned()],
        |list| strings(list),
    )))
}

fn manifest_name(dir: &Path) -> Result<Manifest, Warning> {
    Ok(manifest(dir)?.map_or(Manifest::Absent, |document| {
        Manifest::named(document["name"].as_str())
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::declarations;

    #[test]
    fn every_declaration_names_its_members_and_their_manifest_names() {
        let root = std::env::temp_dir().join(format!("runner-ws-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (dir, name) in [("apps/web", "web"), ("packages/lib", "@acme/lib")] {
            fs::create_dir_all(root.join(dir)).expect("member dir");
            fs::write(
                root.join(dir).join("package.json"),
                format!(r#"{{ "name": "{name}" }}"#),
            )
            .expect("manifest");
        }
        fs::create_dir_all(root.join("apps").join("empty")).expect("empty dir");
        fs::write(root.join("package.json"), r#"{ "workspaces": ["apps/*"] }"#)
            .expect("root manifest");
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )
        .expect("pnpm-workspace.yaml");
        let found = declarations(&root).expect("declarations");
        let kinds: Vec<&str> = found.iter().map(|declaration| declaration.kind).collect();
        assert_eq!(kinds, ["pnpm-workspace.yaml", "package.json workspaces"]);
        assert_eq!(
            found[0].members,
            [(
                Some("@acme/lib".to_owned()),
                root.join("packages").join("lib")
            )]
        );
        assert_eq!(
            found[1].members,
            [(Some("web".to_owned()), root.join("apps").join("web"))]
        );
        let _ = fs::remove_dir_all(PathBuf::from(&root));
    }
}
