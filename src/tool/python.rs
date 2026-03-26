//! Shared Python tooling helpers.

use std::path::Path;

/// Common Python artifact and cache directories.
pub(crate) const CLEAN_DIRS: &[&str] = &[
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".tox",
    ".nox",
    "build",
    "dist",
];

const PROJECT_MARKERS: &[&str] = &[
    "setup.py",
    "requirements.txt",
    "Pipfile",
    "Pipfile.lock",
    "poetry.lock",
    "uv.lock",
];

/// Detected via common Python project markers.
pub(crate) fn detect(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|name| dir.join(name).exists()) || has_python_pyproject(dir)
}

/// Existing Python cleanup targets under the project root.
pub(crate) fn clean_dirs(dir: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = CLEAN_DIRS
        .iter()
        .filter(|name| dir.join(name).is_dir())
        .map(|name| (*name).to_string())
        .collect();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };

            if name.ends_with(".egg-info") {
                dirs.push(name.to_string());
            }
        }
    }

    dirs.sort_unstable();
    dirs.dedup();
    dirs
}

fn has_python_pyproject(dir: &Path) -> bool {
    let path = dir.join("pyproject.toml");
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };

    content.lines().any(|line| {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        matches!(
            trimmed,
            "[project]" | "[build-system]" | "[tool.poetry]" | "[tool.uv]"
        )
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{clean_dirs, detect};
    use crate::tool::test_support::TempDir;

    #[test]
    fn detect_recognizes_generic_python_projects() {
        let dir = TempDir::new("python-detect");
        fs::write(dir.path().join("requirements.txt"), "pytest\n")
            .expect("requirements.txt should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn detect_ignores_tool_only_pyproject() {
        let dir = TempDir::new("python-tool-only");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.ruff]\nline-length = 88\n",
        )
        .expect("pyproject.toml should be written");

        assert!(!detect(dir.path()));
    }

    #[test]
    fn clean_dirs_include_build_outputs_and_egg_info() {
        let dir = TempDir::new("python-clean");
        for name in ["dist", "build", ".tox", ".nox", "demo.egg-info"] {
            fs::create_dir(dir.path().join(name)).expect("python artifact dir should be created");
        }

        assert_eq!(
            clean_dirs(dir.path()),
            vec![
                ".nox".to_string(),
                ".tox".to_string(),
                "build".to_string(),
                "demo.egg-info".to_string(),
                "dist".to_string(),
            ]
        );
    }
}
