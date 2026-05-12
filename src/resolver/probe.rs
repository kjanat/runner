//! `PATH` probe — step 7 of the resolution chain.
//!
//! When no manifest, lockfile, or override signal points the resolver at a
//! package manager, this module walks `$PATH` (and `PATHEXT` on Windows)
//! to discover what is actually installed. The Node ecosystem returns the
//! first match in canonical order — `bun > pnpm > yarn > npm` — matching
//! the priority used elsewhere in detection. Phase 8 (this same step) is
//! what replaces the silent `npm` fallback baked into the resolver since
//! day one.

use std::path::PathBuf;

use crate::types::PackageManager;

/// Probe `$PATH` for `name`. Returns the absolute path of the first
/// matching executable, or `None` if nothing is found.
///
/// On Windows, also walks `PATHEXT` so that `cmd`/`bat` shims are found —
/// the same approach used by [`crate::tool::program::command`].
pub(crate) fn probe(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    probe_in(name, &path, std::env::var_os("PATHEXT").as_deref())
}

/// Pure-function variant for tests. Takes `path` and (optionally)
/// `pathext` directly so the search can be exercised against a temporary
/// directory.
pub(crate) fn probe_in(
    name: &str,
    path: &std::ffi::OsStr,
    pathext: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    use std::path::Path;

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

        // PATHEXT only applies when there's no explicit extension on `name`.
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

/// Canonical PATH-probe order for the package managers that can dispatch
/// Node `package.json` scripts. Matches the lockfile priority used in
/// `detect::detect_package_managers` so that the implicit pick lines up
/// with what the user would see if any one of them had created a lockfile.
pub(crate) const NODE_PROBE_ORDER: &[PackageManager] = &[
    PackageManager::Bun,
    PackageManager::Pnpm,
    PackageManager::Yarn,
    PackageManager::Npm,
];

/// Probe the canonical PM list and return the first installed match.
pub(crate) fn probe_first(order: &[PackageManager]) -> Option<(PackageManager, PathBuf)> {
    order
        .iter()
        .find_map(|&pm| probe(pm.label()).map(|path| (pm, path)))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::{NODE_PROBE_ORDER, probe_in};
    use crate::tool::test_support::TempDir;

    #[test]
    fn probe_in_finds_executable_by_bare_name() {
        let dir = TempDir::new("probe-bare");
        let target = dir.path().join("pnpm");
        fs::write(&target, "#!/bin/sh\n").expect("shim should be written");

        let resolved = probe_in("pnpm", &OsString::from(dir.path()), None)
            .expect("pnpm should resolve via bare name");
        assert!(resolved.ends_with("pnpm"));
    }

    #[test]
    fn probe_in_returns_none_when_path_is_empty() {
        assert!(probe_in("pnpm", &OsString::new(), None).is_none());
    }

    #[test]
    fn probe_in_skips_directories() {
        // A directory entry shouldn't be reported as the binary.
        let dir = TempDir::new("probe-dir");
        fs::create_dir(dir.path().join("yarn")).expect("yarn dir should be created");

        assert!(probe_in("yarn", &OsString::from(dir.path()), None).is_none());
    }

    #[test]
    fn probe_in_finds_pathext_shim_on_windows_style_input() {
        let dir = TempDir::new("probe-pathext");
        let shim = dir.path().join("npm.CMD");
        fs::write(&shim, "@echo off\n").expect("shim should be written");

        let resolved = probe_in(
            "npm",
            &OsString::from(dir.path()),
            Some(&OsString::from(".COM;.EXE;.BAT;.CMD")),
        )
        .expect("npm.CMD should resolve via PATHEXT");
        assert!(resolved.ends_with("npm.CMD"));
    }

    #[test]
    fn probe_in_rejects_names_with_path_separators() {
        let dir = TempDir::new("probe-sep");
        let target = dir.path().join("nested").join("pnpm");
        fs::create_dir_all(target.parent().expect("parent")).expect("parent dir");
        fs::write(&target, "").expect("shim should be written");

        // `nested/pnpm` is not a bare name; CreateProcess / execve handle
        // those directly, so the probe declines.
        assert!(probe_in("nested/pnpm", &OsString::from(dir.path()), None).is_none());
    }

    #[test]
    fn node_probe_order_is_bun_first() {
        // The probe order has to match the lockfile-priority order used in
        // detect::detect_package_managers so the implicit pick lines up
        // with what the user would see if any of them had created a
        // lockfile. Lockfile priority is bun > pnpm > yarn > npm; assert
        // the probe matches.
        assert_eq!(
            NODE_PROBE_ORDER,
            &[
                crate::types::PackageManager::Bun,
                crate::types::PackageManager::Pnpm,
                crate::types::PackageManager::Yarn,
                crate::types::PackageManager::Npm,
            ]
        );
    }
}
