//! `PATH` and `PATHEXT` search with memoisation.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The first executable named `name` in `path`, trying each `PATHEXT` suffix
/// after the bare name. `None` for a name with a path separator.
#[must_use]
pub fn probe_in(name: &str, path: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() || Path::new(name).components().count() > 1 {
        return None;
    }
    let exts: Vec<String> = pathext
        .map(|pe| {
            pe.to_string_lossy()
                .split(';')
                .filter(|e| !e.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let has_explicit_extension = Path::new(name).extension().is_some();
    for dir in std::env::split_paths(path) {
        let bare = dir.join(name);
        if bare.is_file() {
            return Some(bare);
        }
        if has_explicit_extension {
            continue;
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// `name` in `extra` first, then the process `PATH`.
#[must_use]
pub fn probe_with(name: &str, extra: &[PathBuf]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let search = if extra.is_empty() {
        path
    } else {
        std::env::join_paths(extra.iter().cloned().chain(std::env::split_paths(&path)))
            .unwrap_or(path)
    };
    probe_in(name, &search, std::env::var_os("PATHEXT").as_deref())
}

/// A memoising prober over the process `PATH`.
#[derive(Debug, Default)]
pub struct Prober {
    found: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl Prober {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `name` on `PATH`, probed once per name.
    #[must_use]
    pub fn probe(&self, name: &str) -> Option<PathBuf> {
        let mut found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        found
            .entry(name.to_owned())
            .or_insert_with(|| probe_with(name, &[]))
            .clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{Prober, probe_in};

    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "runner-core-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            fs::create_dir_all(&dir).expect("temp dir should be created");
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bare_names_resolve_and_directories_are_skipped() {
        let dir = TempDir::new("probe");
        fs::write(dir.path().join("pnpm"), "#!/bin/sh\n").expect("shim");
        fs::create_dir(dir.path().join("yarn")).expect("dir");
        let path = OsString::from(dir.path());
        assert!(probe_in("pnpm", &path, None).is_some_and(|p| p.ends_with("pnpm")));
        assert!(probe_in("yarn", &path, None).is_none());
        assert!(probe_in("nested/pnpm", &path, None).is_none());
        assert!(probe_in("pnpm", &OsString::new(), None).is_none());
    }

    #[test]
    fn pathext_suffixes_are_tried_after_the_bare_name() {
        let dir = TempDir::new("pathext");
        fs::write(dir.path().join("npm.CMD"), "@echo off\n").expect("shim");
        let found = probe_in(
            "npm",
            &OsString::from(dir.path()),
            Some(&OsString::from(".COM;.EXE;.BAT;.CMD")),
        );
        assert!(found.is_some_and(|p| p.ends_with("npm.CMD")));
    }

    #[test]
    fn a_prober_answers_the_same_twice() {
        let prober = Prober::new();
        assert_eq!(
            prober.probe("runner-core-no-such-program"),
            prober.probe("runner-core-no-such-program")
        );
    }
}
