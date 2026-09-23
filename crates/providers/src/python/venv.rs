//! Where a Python project's executables live.

use std::path::{Path, PathBuf};

/// `.venv/bin` or `.venv/Scripts` under `dir`, then the active
/// `VIRTUAL_ENV`, whichever exist.
#[must_use]
pub fn bin_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut envs = vec![dir.join(".venv")];
    if let Some(active) = std::env::var_os("VIRTUAL_ENV") {
        envs.push(PathBuf::from(active));
    }
    envs.into_iter()
        .flat_map(|env| [env.join("bin"), env.join("Scripts")])
        .filter(|bin| bin.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::bin_dirs;

    #[test]
    fn only_existing_bin_dirs_are_reported() {
        let dir = std::env::temp_dir().join(format!("runner-venv-{}", std::process::id()));
        let bin = dir.join(".venv").join("bin");
        fs::create_dir_all(&bin).expect("venv bin");
        let found = bin_dirs(&dir);
        assert!(found.contains(&bin));
        assert!(!found.iter().any(|d| d.ends_with("Scripts")));
        let _ = fs::remove_dir_all(&dir);
    }
}
