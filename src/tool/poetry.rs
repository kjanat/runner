//! Poetry — Python dependency manager.

use std::path::Path;
use std::process::Command;

/// Detected via `poetry.lock` or Poetry markers in `pyproject.toml`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join("poetry.lock").exists()
        || read_pyproject(dir).is_some_and(|content| is_poetry_pyproject(&content))
}

fn read_pyproject(dir: &Path) -> Option<String> {
    let path = dir.join("pyproject.toml");
    if !path.exists() {
        return None;
    }

    std::fs::read_to_string(path).ok()
}

fn is_poetry_pyproject(content: &str) -> bool {
    let mut in_build_system = false;

    for line in content.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_build_system = trimmed == "[build-system]";
            if trimmed == "[tool.poetry]" {
                return true;
            }
            continue;
        }

        if in_build_system
            && trimmed.starts_with("build-backend")
            && trimmed.contains("poetry.core.masonry.api")
        {
            return true;
        }
    }

    false
}

/// `poetry install`
pub(crate) fn install_cmd() -> Command {
    let mut c = Command::new("poetry");
    c.arg("install");
    c
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::detect;
    use crate::tool::test_support::TempDir;

    #[test]
    fn detects_poetry_lockfile() {
        let dir = TempDir::new("poetry-lock");
        fs::write(dir.path().join("poetry.lock"), "").expect("poetry.lock should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn detects_tool_poetry_pyproject() {
        let dir = TempDir::new("poetry-table");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("pyproject.toml should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn detects_poetry_build_backend_without_tool_table() {
        let dir = TempDir::new("poetry-backend");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[build-system]\nrequires = [\"poetry-core>=1.8.0\"]\nbuild-backend = \"poetry.core.masonry.api\"\n",
        )
        .expect("pyproject.toml should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn ignores_generic_pyproject() {
        let dir = TempDir::new("poetry-generic");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[build-system]\nrequires = [\"hatchling\"]\nbuild-backend = \"hatchling.build\"\n",
        )
        .expect("pyproject.toml should be written");

        assert!(!detect(dir.path()));
    }
}
