//! Small filesystem helpers shared by tool detectors/parsers.

use std::path::{Path, PathBuf};

/// Return the first existing file in `dir` matching `filenames` order.
pub(crate) fn find_first(dir: &Path, filenames: &[&str]) -> Option<PathBuf> {
    filenames
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
}

/// Return the first existing file matching `filenames` while walking upward.
pub(crate) fn find_first_upwards(dir: &Path, filenames: &[&str]) -> Option<PathBuf> {
    let mut ancestors = dir.ancestors();

    if let Some(boundary) = vcs_root(dir) {
        ancestors
            .by_ref()
            .take_while(|ancestor| starts_with_boundary(ancestor, &boundary))
            .find_map(|ancestor| find_first(ancestor, filenames))
    } else {
        ancestors.find_map(|ancestor| find_first(ancestor, filenames))
    }
}

fn vcs_root(dir: &Path) -> Option<PathBuf> {
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
    use crate::tool::test_support::TempDir;

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
