//! Small filesystem helpers shared by tool detectors/parsers.

use std::path::{Path, PathBuf};

/// Return the first existing file in `dir` matching `filenames` order.
#[must_use]
pub fn find_first(dir: &Path, filenames: &[&str]) -> Option<PathBuf> {
    filenames
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}

/// The first `Some` from `pred` over `dir` and its ancestors.
///
/// The walk stops at the enclosing VCS root when there is one.
pub fn find_in_ancestors<T>(dir: &Path, mut pred: impl FnMut(&Path) -> Option<T>) -> Option<T> {
    let mut ancestors = dir.ancestors();

    if let Some(boundary) = vcs_root(dir) {
        ancestors
            .by_ref()
            .take_while(|ancestor| starts_with_boundary(ancestor, &boundary))
            .find_map(&mut pred)
    } else {
        ancestors.find_map(&mut pred)
    }
}

/// Return the first existing file matching `filenames` while walking upward.
#[must_use]
pub fn find_first_upwards(dir: &Path, filenames: &[&str]) -> Option<PathBuf> {
    find_in_ancestors(dir, |ancestor| find_first(ancestor, filenames))
}

/// The nearest Git or Jujutsu repository boundary.
pub fn vcs_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|ancestor| ancestor.join(".jj").is_dir() || ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn starts_with_boundary(path: &Path, boundary: &Path) -> bool {
    path == boundary || path.starts_with(boundary)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::find_first_upwards;
    use crate::extract::test_support::TempDir;

    #[test]
    fn find_first_upwards_stops_at_git_root() {
        let outer = TempDir::new("files-upwards-boundary-outer");
        let repo = outer.path().join("repo");
        let nested = repo.join("apps").join("site").join("src");
        fs::create_dir_all(repo.join(".git")).expect("git dir should be created");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(outer.path().join("deno.json"), "{}").expect("outer deno.json should be written");

        assert_eq!(find_first_upwards(&nested, &["deno.json"]), None);
    }
}
