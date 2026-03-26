//! Shared Node.js helpers used by all Node package managers.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context as _;
use serde::Deserialize;

use crate::types::PackageManager;

/// Directories commonly produced by Node.js toolchains.
pub(crate) const DEFAULT_CLEAN_DIRS: &[&str] = &["node_modules", ".cache", "dist"];

/// Framework-specific Node build directories removed only on explicit opt-in.
pub(crate) const FRAMEWORK_CLEAN_DIRS: &[&str] = &[".next", ".parcel-cache", ".svelte-kit"];

/// Returns `true` if `dir` contains a `package.json`.
pub(crate) fn has_package_json(dir: &Path) -> bool {
    dir.join("package.json").exists()
}

/// Detect the Node package manager from the `"packageManager"` field in
/// `package.json`. Falls back to [`PackageManager::Npm`] when absent or
/// unparseable.
pub(crate) fn detect_pm_from_field(dir: &Path) -> PackageManager {
    match parse_package_json(dir)
        .and_then(|package_json| package_json.package_manager)
        .as_deref()
    {
        Some(s) if s.starts_with("pnpm") => PackageManager::Pnpm,
        Some(s) if s.starts_with("yarn") => PackageManager::Yarn,
        Some(s) if s.starts_with("bun") => PackageManager::Bun,
        _ => PackageManager::Npm,
    }
}

/// Parse `package.json` and return all keys from the `"scripts"` object.
pub(crate) fn extract_scripts(dir: &Path) -> anyhow::Result<Vec<String>> {
    let Some(content) = read_package_json(dir)? else {
        return Ok(vec![]);
    };

    let package_json = serde_json::from_str::<PackageJson>(&content)
        .with_context(|| format!("{} is not valid JSON", dir.join("package.json").display()))?;

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
    let content = read_package_json(dir).ok()??;
    serde_json::from_str(&content).ok()
}

fn read_package_json(dir: &Path) -> anyhow::Result<Option<String>> {
    let path = dir.join("package.json");
    if !path.exists() {
        return Ok(None);
    }

    std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))
        .map(Some)
}
