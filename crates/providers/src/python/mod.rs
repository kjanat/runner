//! The Python ecosystem.

pub mod pipenv;
pub mod poetry;
pub mod pyproject;
pub mod uv;
pub mod venv;

use std::path::Path;

use runner_core::{CleanCap, Template, t};

/// What `clean` removes for any Python project.
pub const CLEAN: CleanCap = CleanCap {
    dir_suffixes: &[".egg-info"],
    framework_dirs: &[],
    dirs: &[
        ".venv",
        "__pycache__",
        ".mypy_cache",
        ".ruff_cache",
        ".pytest_cache",
        ".tox",
        ".nox",
        "build",
        "dist",
    ],
};

/// The interpreter that runs a `.py` file outside a managed environment.
pub const INTERPRETER: &str = if cfg!(windows) { "python" } else { "python3" };

/// The test runner a Python project reaches for, most specific first: the one
/// its virtualenv holds, then the one its config implies, then `unittest`,
/// which ships with Python.
pub fn test_runner(dir: &Path) -> std::io::Result<Option<Template>> {
    const PYTEST: Template = t!["run", "pytest", Args];
    const NOSE2: Template = t!["run", "nose2", Args];
    const WARD: Template = t!["run", "ward", Args];
    const DJANGO: Template = t!["run", "python", "manage.py", "test", Args];
    const TOX: Template = t!["run", "tox", Args];
    const NOX: Template = t!["run", "nox", Args];
    const UNITTEST: Template = t!["run", "python", "-m", "unittest", Args];

    let bins = venv::bin_dirs(dir)?;
    let installed = |name: &str| {
        bins.iter()
            .any(|bin| bin.join(name).is_file() || bin.join(format!("{name}.exe")).is_file())
    };
    let file = |name: &str| dir.join(name).is_file();
    let available = |name: &str| installed(name) || runner_core::probe_with(name, &bins).is_some();

    if installed("pytest") || file("pytest.ini") || file("conftest.py") {
        return Ok(Some(PYTEST));
    }
    if installed("nose2") {
        return Ok(Some(NOSE2));
    }
    if installed("ward") {
        return Ok(Some(WARD));
    }
    if file("manage.py") {
        return Ok(Some(DJANGO));
    }
    if file("tox.ini") && available("tox") {
        return Ok(Some(TOX));
    }
    if file("noxfile.py") && available("nox") {
        return Ok(Some(NOX));
    }
    Ok(Some(UNITTEST))
}

pub mod runtime;
