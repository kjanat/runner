//! Shared Node.js helpers used by all Node package managers.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::types::PackageManager;

/// Directories commonly produced by Node.js toolchains.
pub(crate) const CLEAN_DIRS: &[&str] = &[
    "node_modules",
    ".next",
    "dist",
    ".cache",
    ".parcel-cache",
    ".svelte-kit",
];

/// Returns `true` if `dir` contains a `package.json`.
pub(crate) fn has_package_json(dir: &Path) -> bool {
    dir.join("package.json").exists()
}

/// Detect the Node package manager from the `"packageManager"` field in
/// `package.json`. Falls back to [`PackageManager::Npm`] when absent or
/// unparseable.
pub(crate) fn detect_pm_from_field(dir: &Path) -> PackageManager {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(rename = "packageManager")]
        package_manager: Option<String>,
    }
    let Ok(content) = std::fs::read_to_string(dir.join("package.json")) else {
        return PackageManager::Npm;
    };
    let Ok(p) = serde_json::from_str::<Partial>(&content) else {
        return PackageManager::Npm;
    };
    match p.package_manager.as_deref() {
        Some(s) if s.starts_with("pnpm") => PackageManager::Pnpm,
        Some(s) if s.starts_with("yarn") => PackageManager::Yarn,
        Some(s) if s.starts_with("bun") => PackageManager::Bun,
        _ => PackageManager::Npm,
    }
}

/// Parse `package.json` and return all keys from the `"scripts"` object.
pub(crate) fn extract_scripts(dir: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct Partial {
        scripts: Option<HashMap<String, String>>,
    }
    let Ok(content) = std::fs::read_to_string(dir.join("package.json")) else {
        return vec![];
    };
    let Ok(p) = serde_json::from_str::<Partial>(&content) else {
        return vec![];
    };
    p.scripts.map_or(vec![], |s| s.into_keys().collect())
}
