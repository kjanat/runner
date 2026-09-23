//! Cargo, the Rust package manager and build tool.

use std::path::Path;

/// Directories produced by Cargo builds.
pub(crate) const CLEAN_DIRS: &[&str] = &["target"];

/// Detected via `Cargo.toml`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("Cargo.toml").exists()
}

/// Returns `true` if `Cargo.toml` contains a top-level `[workspace]` table.
///
/// Uses a line-anchored check to avoid false positives from
/// `[workspace.dependencies]` or comments.
pub(crate) fn detect_workspace(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|c| {
        c.lines().any(|line| {
            line.split('#')
                .next()
                .is_some_and(|part| part.trim() == "[workspace]")
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::detect_workspace;
    use crate::tool::test_support::TempDir;

    #[test]
    fn workspace_detection_allows_inline_comments() {
        let dir = TempDir::new("cargo-workspace");

        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace] # root\nmembers = []\n",
        )
        .expect("Cargo.toml should be written");

        assert!(detect_workspace(dir.path()));
    }
}
