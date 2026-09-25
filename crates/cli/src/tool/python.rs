//! Shared Python tooling helpers.

use std::path::{Path, PathBuf};

/// Bare interpreter name for running a `.py` file directly when no uv
/// project is detected. Windows ships `python`; most other hosts expose
/// the interpreter as `python3` (and reserve bare `python` for legacy
/// Python 2 or leave it unset).
pub(crate) const PYTHON_BIN: &str = if cfg!(windows) { "python" } else { "python3" };

const PYPROJECT_FILENAMES: &[&str] = &["pyproject.toml"];

/// Find the nearest `pyproject.toml` at `dir` or above, bounded by the
/// containing VCS root when one exists.
pub(crate) fn find_pyproject_upwards(dir: &Path) -> Option<PathBuf> {
    super::files::find_first_upwards(dir, PYPROJECT_FILENAMES)
}
