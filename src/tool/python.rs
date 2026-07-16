//! Shared Python tooling helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

/// Bare interpreter name for running a `.py` file directly when no uv
/// project is detected. Windows ships `python`; most other hosts expose
/// the interpreter as `python3` (and reserve bare `python` for legacy
/// Python 2 or leave it unset).
pub(crate) const PYTHON_BIN: &str = if cfg!(windows) { "python" } else { "python3" };

/// `<python> <file> [args...]`, execute a local Python script with the
/// system interpreter. The uv path (`uv run <file>`) is preferred when a
/// uv project is detected; this is the plain fallback.
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command(PYTHON_BIN);
    c.arg(file).args(args);
    c
}

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

const PYPROJECT_FILENAMES: &[&str] = &["pyproject.toml"];

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

/// Find the nearest `pyproject.toml` at `dir` or above, bounded by the
/// containing VCS root when one exists.
pub(crate) fn find_pyproject_upwards(dir: &Path) -> Option<PathBuf> {
    super::files::find_first_upwards(dir, PYPROJECT_FILENAMES)
}

/// Extract `[project.scripts]` entry points (PEP 621 console scripts)
/// from `pyproject.toml`, each paired with its entry-point target as a
/// description (e.g. `("greenpy", Some("greenpy.main:main"))`).
///
/// These are surfaced as tasks and dispatched via the detected Python
/// PM's `run` subcommand (`uv run <name>`, `poetry run <name>`,
/// `pipenv run <name>`), the same way the script's installed console
/// entry point would be invoked inside the project environment.
///
/// Returns an empty list when `pyproject.toml` is absent or declares no
/// `[project.scripts]`; errors only when the file exists but can't be
/// read or parsed, so a malformed manifest surfaces as a
/// `TaskListUnreadable` warning rather than silently dropping tasks.
pub(crate) fn extract_pyproject_scripts(
    dir: &Path,
) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let Some(path) = find_pyproject_upwards(dir) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: PyprojectDoc =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;

    // `BTreeMap` iterates in sorted key order, so the returned list is
    // already alphabetized, matching the post-extraction sort that
    // `detect::detect` applies to the full task list.
    Ok(doc
        .project
        .and_then(|project| project.scripts)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, target)| (name, Some(target)))
        .collect())
}

#[derive(Deserialize)]
struct PyprojectDoc {
    project: Option<PyprojectProject>,
}

#[derive(Deserialize)]
struct PyprojectProject {
    scripts: Option<BTreeMap<String, String>>,
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

    use super::{PYTHON_BIN, clean_dirs, detect, extract_pyproject_scripts, run_file_cmd};
    use crate::tool::test_support::TempDir;

    #[test]
    fn run_file_cmd_uses_interpreter_with_file() {
        use std::path::Path;

        let cmd = run_file_cmd(Path::new("/abs/tool.py"), &[String::from("--check")]);
        let built: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), PYTHON_BIN);
        assert_eq!(built, ["/abs/tool.py", "--check"]);
    }

    #[test]
    fn extract_scripts_returns_names_with_entry_point_targets_sorted() {
        let dir = TempDir::new("pyproject-scripts");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"greenpy\"\n\n[project.scripts]\ngreenpy = \
             \"greenpy.main:main\"\nbodysuit = \"greenpy.bodysuit:main\"\n",
        )
        .expect("pyproject.toml should be written");

        let scripts = extract_pyproject_scripts(dir.path()).expect("scripts should parse");

        assert_eq!(
            scripts,
            [
                (
                    "bodysuit".to_string(),
                    Some("greenpy.bodysuit:main".to_string())
                ),
                ("greenpy".to_string(), Some("greenpy.main:main".to_string())),
            ]
        );
    }

    #[test]
    fn extract_scripts_returns_empty_without_scripts_table() {
        let dir = TempDir::new("pyproject-no-scripts");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"greenpy\"\nversion = \"0.1.0\"\n",
        )
        .expect("pyproject.toml should be written");

        assert!(
            extract_pyproject_scripts(dir.path())
                .expect("scripts should parse")
                .is_empty()
        );
    }

    #[test]
    fn extract_scripts_returns_empty_without_pyproject() {
        let dir = TempDir::new("pyproject-missing");
        assert!(
            extract_pyproject_scripts(dir.path())
                .expect("absent file is not an error")
                .is_empty()
        );
    }

    #[test]
    fn extract_scripts_reads_nearest_upward_pyproject() {
        let dir = TempDir::new("pyproject-upwards");
        let nested = dir.path().join("src").join("pkg");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"greenpy\"\n\n[project.scripts]\ngreenpy = \"greenpy.main:main\"\n",
        )
        .expect("pyproject.toml should be written");

        let scripts = extract_pyproject_scripts(&nested).expect("scripts should parse");

        assert_eq!(
            scripts,
            [("greenpy".to_string(), Some("greenpy.main:main".to_string()))]
        );
    }

    #[test]
    fn extract_scripts_surfaces_parse_error_for_malformed_toml() {
        let dir = TempDir::new("pyproject-malformed");
        fs::write(dir.path().join("pyproject.toml"), "[project.scripts")
            .expect("pyproject.toml should be written");

        let err = extract_pyproject_scripts(dir.path())
            .expect_err("malformed pyproject.toml should error");

        assert!(
            err.to_string().contains("failed to parse"),
            "error chain should mention parse failure: {err:#}"
        );
    }

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
