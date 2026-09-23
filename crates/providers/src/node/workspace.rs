//! Workspace members declared at a Node root.

use std::path::{Component, Path, PathBuf};

use runner_core::{Scope, Tree, Warning};
use serde_json::Value;
use yaml_rust2::YamlLoader;

/// The members `package.json` `workspaces`, `pnpm-workspace.yaml` and
/// `lerna.json` declare under the root, each carrying a manifest.
///
/// # Errors
///
/// When a declaration file exists but cannot be read.
pub fn members(tree: &Tree) -> Result<Vec<Scope>, Warning> {
    let root = tree.root.as_path();
    let mut globs = Vec::new();
    globs.extend(pnpm_globs(root)?);
    globs.extend(package_json_globs(root)?);
    globs.extend(lerna_globs(root)?);
    let mut scopes = Vec::new();
    for dir in expand(root, &globs) {
        if !dir.join("package.json").is_file()
            || scopes
                .iter()
                .any(|s| matches!(s, Scope::Member { dir: d, .. } if *d == dir))
        {
            continue;
        }
        let name = manifest_name(&dir).unwrap_or_else(|| relative(root, &dir));
        scopes.push(Scope::Member { name, dir });
    }
    Ok(scopes)
}

fn read(root: &Path, file: &str) -> Result<Option<String>, Warning> {
    let path = root.join(file);
    if !path.is_file() {
        return Ok(None);
    }
    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|err| Warning::general(format!("{}: {err}", path.display())))
}

fn pnpm_globs(root: &Path) -> Result<Vec<String>, Warning> {
    let Some(text) = read(root, "pnpm-workspace.yaml")? else {
        return Ok(Vec::new());
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
        })
        .unwrap_or_default())
}

fn package_json_globs(root: &Path) -> Result<Vec<String>, Warning> {
    let Some(text) = read(root, "package.json")? else {
        return Ok(Vec::new());
    };
    let manifest: Value = serde_json::from_str(&text)
        .map_err(|err| Warning::general(format!("package.json: {err}")))?;
    let workspaces = match &manifest["workspaces"] {
        Value::Array(list) => list.clone(),
        Value::Object(map) => map
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Ok(workspaces
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect())
}

fn lerna_globs(root: &Path) -> Result<Vec<String>, Warning> {
    let Some(text) = read(root, "lerna.json")? else {
        return Ok(Vec::new());
    };
    let lerna: Value = serde_json::from_str(&text)
        .map_err(|err| Warning::general(format!("lerna.json: {err}")))?;
    if lerna["useWorkspaces"].as_bool() == Some(true) {
        return Ok(Vec::new());
    }
    Ok(lerna["packages"].as_array().map_or_else(
        || vec!["packages/*".to_owned()],
        |list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        },
    ))
}

fn manifest_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: Value = serde_json::from_str(&text).ok()?;
    manifest["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn relative(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .filter_map(|c| match c {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: true,
};

fn expand(root: &Path, globs: &[String]) -> Vec<PathBuf> {
    let (negatives, positives): (Vec<&str>, Vec<&str>) = globs
        .iter()
        .map(String::as_str)
        .partition(|g| g.starts_with('!'));
    let negatives: Vec<glob::Pattern> = negatives
        .iter()
        .filter_map(|g| glob::Pattern::new(&normalize(&g[1..])).ok())
        .collect();
    let escaped_root = glob::Pattern::escape(&root.to_string_lossy());
    let mut dirs: Vec<PathBuf> = Vec::new();
    for positive in positives {
        let pattern = normalize(positive);
        if pattern.is_empty() {
            continue;
        }
        let Ok(paths) = glob::glob_with(&format!("{escaped_root}/{pattern}"), MATCH_OPTIONS) else {
            continue;
        };
        for path in paths.filter_map(Result::ok) {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let in_node_modules = rel.components().any(|c| c.as_os_str() == "node_modules");
            let excluded = negatives
                .iter()
                .any(|n| n.matches_path_with(rel, MATCH_OPTIONS));
            if path.is_dir() && !in_node_modules && !excluded && !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    dirs.sort();
    dirs
}

fn normalize(glob: &str) -> String {
    glob.trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use runner_core::{Scope, Tree};

    use super::members;

    #[test]
    fn members_come_from_every_declaration_and_carry_their_manifest_name() {
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
        let tree = Tree {
            cwd: root.clone(),
            root: root.clone(),
            members: Vec::new(),
        };
        let found = members(&tree).expect("members");
        assert_eq!(
            found,
            [
                Scope::Member {
                    name: "web".to_owned(),
                    dir: root.join("apps").join("web"),
                },
                Scope::Member {
                    name: "@acme/lib".to_owned(),
                    dir: root.join("packages").join("lib"),
                },
            ]
        );
        let _ = fs::remove_dir_all(PathBuf::from(&root));
    }
}
