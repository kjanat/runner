//! Shared Node.js helpers used by all Node package managers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;
use yaml_rust2::YamlLoader;

use crate::tool::files;
use crate::types::PackageManager;

/// Node manifest filename.
pub(crate) const PACKAGE_JSON_FILENAME: &str = "package.json";

/// Supported Node manifest filenames, in resolution order.
pub(crate) const MANIFEST_FILENAMES: &[&str] =
    &[PACKAGE_JSON_FILENAME, "package.json5", "package.yaml"];

/// Directories commonly produced by Node.js toolchains.
pub(crate) const DEFAULT_CLEAN_DIRS: &[&str] = &["node_modules", ".cache", "dist"];

/// Framework-specific Node build directories removed only on explicit opt-in.
pub(crate) const FRAMEWORK_CLEAN_DIRS: &[&str] = &[".next", ".parcel-cache", ".svelte-kit"];

/// Returns `true` if `dir` contains a supported package manifest.
pub(crate) fn has_package_json(dir: &Path) -> bool {
    find_manifest(dir).is_some()
}

/// Resolve the first supported package manifest path.
pub(crate) fn find_manifest(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, MANIFEST_FILENAMES).filter(|path| path.is_file())
}

/// Detect the package manager named by the `"packageManager"` field in the
/// supported package manifest.
pub(crate) fn detect_pm_from_field(dir: &Path) -> Option<PackageManager> {
    match parse_package_json(dir)
        .and_then(|package_json| package_json.package_manager)
        .as_deref()
    {
        Some(s) if s.starts_with("npm") => Some(PackageManager::Npm),
        Some(s) if s.starts_with("pnpm") => Some(PackageManager::Pnpm),
        Some(s) if s.starts_with("yarn") => Some(PackageManager::Yarn),
        Some(s) if s.starts_with("bun") => Some(PackageManager::Bun),
        Some(s) if s.starts_with("deno") => Some(PackageManager::Deno),
        _ => None,
    }
}

/// Parse the supported package manifest and return all script names.
pub(crate) fn extract_scripts(dir: &Path) -> anyhow::Result<Vec<String>> {
    let Some((path, content)) = read_manifest(dir)? else {
        return Ok(vec![]);
    };

    let package_json = parse_manifest(&path, &content)
        .with_context(|| format!("{} is not valid {}", path.display(), manifest_format(&path)))?;

    Ok(package_json
        .scripts
        .map_or_else(Vec::new, |scripts| scripts.into_keys().collect()))
}

#[derive(Deserialize)]
struct PackageJson {
    #[serde(rename = "packageManager")]
    package_manager: Option<String>,
    scripts: Option<HashMap<String, String>>,
}

fn parse_package_json(dir: &Path) -> Option<PackageJson> {
    let (path, content) = read_manifest(dir).ok()??;
    parse_manifest(&path, &content)
}

fn read_manifest(dir: &Path) -> anyhow::Result<Option<(PathBuf, String)>> {
    let Some(path) = find_manifest(dir) else {
        return Ok(None);
    };

    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map(|content| Some((path, content)))
}

fn parse_manifest(path: &Path, content: &str) -> Option<PackageJson> {
    if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.json5"))
    {
        json5::from_str(content).ok()
    } else if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.yaml"))
    {
        parse_package_yaml(content)
    } else {
        serde_json::from_str(content).ok()
    }
}

fn parse_package_yaml(content: &str) -> Option<PackageJson> {
    let docs = YamlLoader::load_from_str(content).ok()?;
    let doc = docs.first()?;
    let root = doc.as_hash()?;

    let package_manager = root
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("packageManager")).then_some(value))
        .and_then(yaml_rust2::Yaml::as_str)
        .map(ToOwned::to_owned);

    let scripts = root
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("scripts")).then_some(value))
        .and_then(yaml_rust2::Yaml::as_hash)
        .map(|table| {
            table
                .iter()
                .filter_map(|(name, _)| name.as_str().map(ToOwned::to_owned))
                .map(|name| (name, String::new()))
                .collect::<HashMap<_, _>>()
        })
        .filter(|table| !table.is_empty());

    Some(PackageJson {
        package_manager,
        scripts,
    })
}

fn manifest_format(path: &Path) -> &'static str {
    if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.json5"))
    {
        "JSON5"
    } else if path
        .file_name()
        .is_some_and(|name| name == std::ffi::OsStr::new("package.yaml"))
    {
        "YAML"
    } else {
        "JSON"
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{detect_pm_from_field, extract_scripts};
    use crate::tool::test_support::TempDir;
    use crate::types::PackageManager;

    #[test]
    fn detect_pm_from_field_supports_package_json5() {
        let dir = TempDir::new("node-package-json5-pm");
        fs::write(
            dir.path().join("package.json5"),
            "{ packageManager: 'pnpm@9.0.0' }",
        )
        .expect("package.json5 should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Pnpm));
    }

    #[test]
    fn extract_scripts_supports_package_json5() {
        let dir = TempDir::new("node-package-json5-scripts");
        fs::write(
            dir.path().join("package.json5"),
            "{ scripts: { build: 'vite build', test: 'vitest' } }",
        )
        .expect("package.json5 should be written");

        let mut scripts =
            extract_scripts(dir.path()).expect("scripts should parse from package.json5");
        scripts.sort_unstable();

        assert_eq!(scripts, ["build", "test"]);
    }

    #[test]
    fn detect_pm_from_field_supports_package_yaml() {
        let dir = TempDir::new("node-package-yaml-pm");
        fs::write(
            dir.path().join("package.yaml"),
            "packageManager: yarn@4.3.0\n",
        )
        .expect("package.yaml should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Yarn));
    }

    #[test]
    fn detect_pm_from_field_supports_deno_package_manager() {
        let dir = TempDir::new("node-package-json-deno-pm");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "deno@2.7.12" }"#,
        )
        .expect("package.json should be written");

        assert_eq!(detect_pm_from_field(dir.path()), Some(PackageManager::Deno));
    }

    #[test]
    fn extract_scripts_supports_package_yaml() {
        let dir = TempDir::new("node-package-yaml-scripts");
        fs::write(
            dir.path().join("package.yaml"),
            "scripts:\n  build: vite build\n  test: vitest\n",
        )
        .expect("package.yaml should be written");

        let mut scripts =
            extract_scripts(dir.path()).expect("scripts should parse from package.yaml");
        scripts.sort_unstable();

        assert_eq!(scripts, ["build", "test"]);
    }

    #[test]
    fn extract_scripts_supports_inline_yaml_script_map() {
        let dir = TempDir::new("node-package-yaml-inline-scripts");
        fs::write(
            dir.path().join("package.yaml"),
            "scripts: { build: vite build, test: vitest }\n",
        )
        .expect("package.yaml should be written");

        let mut scripts =
            extract_scripts(dir.path()).expect("scripts should parse from inline YAML map");
        scripts.sort_unstable();

        assert_eq!(scripts, ["build", "test"]);
    }
}
