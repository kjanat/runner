//! Small filesystem helpers shared by tool detectors/parsers.

use std::path::{Path, PathBuf};

/// Return the first existing file in `dir` matching `filenames` order.
pub(crate) fn find_first(dir: &Path, filenames: &[&str]) -> Option<PathBuf> {
    filenames
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}
