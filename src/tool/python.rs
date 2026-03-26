//! Shared Python tooling constants.

/// Common Python artifact directories.
pub(crate) const CLEAN_DIRS: &[&str] = &[
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
];
