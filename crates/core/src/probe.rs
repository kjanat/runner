//! `PATH` and `PATHEXT` search with memoisation.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The first executable named `name` in `path`.
///
/// `PATHEXT` suffixes are tried after the bare name. On Windows a name
/// without an extension only matches through `PATHEXT`. `None` for a name
/// with a path separator.
#[must_use]
pub fn probe_in(name: &str, path: &OsStr, pathext: Option<&OsStr>) -> Option<PathBuf> {
    if name.is_empty() || Path::new(name).components().count() > 1 {
        return None;
    }
    probe_in_dirs(name, std::env::split_paths(path), pathext)
}

/// Search directories without encoding them as a PATH string.
#[must_use]
pub fn probe_in_dirs(
    name: &str,
    dirs: impl IntoIterator<Item = PathBuf>,
    pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    if name.is_empty() || Path::new(name).components().count() > 1 {
        return None;
    }
    let exts: Vec<String> = pathext
        .map_or_else(|| DEFAULT_PATHEXT.into(), OsStr::to_string_lossy)
        .split(';')
        .filter(|e| !e.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let has_explicit_extension = Path::new(name).extension().is_some();
    for dir in dirs {
        let bare = dir.join(name);
        if (has_explicit_extension || !cfg!(windows)) && executable(&bare) {
            return Some(bare);
        }
        if has_explicit_extension {
            continue;
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

const DEFAULT_PATHEXT: &str = if cfg!(windows) {
    ".COM;.EXE;.BAT;.CMD"
} else {
    ""
};

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `name` in `extra` first, then the process `PATH`.
#[must_use]
pub fn probe_with(name: &str, extra: &[PathBuf]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    probe_in_dirs(
        name,
        extra.iter().cloned().chain(std::env::split_paths(&path)),
        std::env::var_os("PATHEXT").as_deref(),
    )
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
        let file = if cfg!(windows) { "pnpm.exe" } else { "pnpm" };
        fs::write(dir.path().join(file), "#!/bin/sh\n").expect("shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path().join(file), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir(dir.path().join("yarn")).expect("dir");
        let path = OsString::from(dir.path());
        assert!(probe_in("pnpm", &path, None).is_some_and(|p| p.ends_with(file)));
        assert!(probe_in("yarn", &path, None).is_none());
        assert!(probe_in("nested/pnpm", &path, None).is_none());
        assert!(probe_in("pnpm", &OsString::new(), None).is_none());
    }

    #[test]
    fn pathext_suffixes_are_tried_after_the_bare_name() {
        let dir = TempDir::new("pathext");
        fs::write(dir.path().join("npm.CMD"), "@echo off\n").expect("shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.path().join("npm.CMD"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let found = probe_in(
            "npm",
            &OsString::from(dir.path()),
            Some(&OsString::from(".COM;.EXE;.BAT;.CMD")),
        );
        assert!(found.is_some_and(|p| p.ends_with("npm.CMD")));
    }

    #[test]
    #[cfg(windows)]
    fn an_extensionless_shim_is_not_a_windows_executable() {
        let dir = TempDir::new("windows-shim");
        fs::write(dir.path().join("just"), "#!/bin/sh\n").expect("shim");
        fs::write(dir.path().join("just.cmd"), "@echo off\n").expect("cmd shim");
        let found = probe_in("just", &OsString::from(dir.path()), None);
        assert!(found.is_some_and(|p| p.ends_with("just.cmd")));
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
