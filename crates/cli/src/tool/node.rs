//! Node.js manifest lookup.

use std::path::{Path, PathBuf};

use runner_providers::node::MANIFESTS;

use crate::tool::files;

/// Returns `true` if `dir` contains a supported package manifest.
pub(crate) fn has_package_json(dir: &Path) -> bool {
    find_manifest(dir).is_some()
}

/// Resolve the first supported package manifest path.
pub(crate) fn find_manifest(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, MANIFESTS).filter(|path| path.is_file())
}

/// Resolve the nearest supported package manifest path while walking upward.
pub(crate) fn find_manifest_upwards(dir: &Path) -> Option<PathBuf> {
    files::find_first_upwards(dir, MANIFESTS).filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::find_manifest_upwards;
    use crate::tool::test_support::TempDir;

    #[test]
    fn find_manifest_upwards_prefers_nearest_manifest() {
        let dir = TempDir::new("node-manifest-upwards");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "root": "1" } }"#,
        )
        .expect("root package.json should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{ "scripts": { "member": "1" } }"#,
        )
        .expect("member package.json should be written");

        let path = find_manifest_upwards(&nested).expect("nearest manifest should resolve");

        assert!(path.ends_with("apps/site/package.json"));
    }
}
