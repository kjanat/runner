//! `PATH` probe — step 7 of the resolution chain.
//!
//! When no manifest, lockfile, or override signal points the resolver at a
//! package manager, this module walks `$PATH` (and `PATHEXT` on Windows)
//! to discover what is actually installed. The Node ecosystem returns the
//! first match in canonical order — `bun > pnpm > yarn > npm` — matching
//! the priority used elsewhere in detection. Phase 8 (this same step) is
//! what replaces the silent `npm` fallback baked into the resolver since
//! day one.
//!
//! ## Caching
//!
//! [`probe`] memoizes results in a static `[OnceLock<Option<PathBuf>>;
//! PackageManager::COUNT]` array indexed by [`PackageManager::index`].
//! Per-name `OnceLock::get_or_init` gives exactly-once probing — even
//! across concurrent callers — without ever holding a lock during the
//! PATH walk: `OnceLock` synchronises on the slot itself, so racing
//! callers all return the same value computed by the first one to
//! win the init race. No `Mutex` held across syscalls; the universe
//! of probed names is closed at compile time, so an array beats a
//! `HashMap` on lookup cost and avoids any allocation after start-up.
//!
//! The pure-function variant [`probe_in`] stays cache-free so tests
//! exercise the search logic against a controlled directory without
//! racing or polluting the shared cache.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::types::PackageManager;

/// Process-wide cache of [`probe`] lookups, one slot per
/// [`PackageManager`] variant. `OnceLock` initialises lazily and
/// guarantees the initialiser runs at most once even when called
/// concurrently — exactly the semantics we want here.
static CACHE: [OnceLock<Option<PathBuf>>; PackageManager::COUNT] =
    [const { OnceLock::new() }; PackageManager::COUNT];

/// Probe `$PATH` for `pm`. Returns the absolute path of the first
/// matching executable, or `None` if nothing is found.
///
/// On Windows, also walks `PATHEXT` so that `cmd`/`bat` shims are found —
/// the same approach used by [`crate::tool::program::command`].
///
/// Result is memoized in [`CACHE`] for the lifetime of the process.
pub(crate) fn probe(pm: PackageManager) -> Option<PathBuf> {
    CACHE[pm.index()]
        .get_or_init(|| {
            std::env::var_os("PATH").and_then(|path| {
                probe_in(pm.label(), &path, std::env::var_os("PATHEXT").as_deref())
            })
        })
        .clone()
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

/// Probe every entry of `order` and return all installed matches in order.
///
/// The first element is the resolver's pick under [`FallbackPolicy::Probe`];
/// the remainder populates `DetectionWarning::PathProbeFallback`'s
/// `others_available` so users can see what else was installed when the
/// resolver picked the first PM by precedence.
pub(crate) fn probe_all(order: &[PackageManager]) -> Vec<(PackageManager, PathBuf)> {
    order
        .iter()
        .filter_map(|&pm| probe(pm).map(|path| (pm, path)))
        .collect()
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
    fn probe_returns_consistent_value_across_calls() {
        // `OnceLock` guarantees the initialiser runs at most once,
        // so subsequent calls to `probe(pm)` return clones of the
        // same cached `Option<PathBuf>`. The slot is process-wide
        // and never cleared (that's the point — no Mutex held across
        // PATH walks); the test asserts the caller-visible property
        // (idempotence) rather than poking at the cache internals,
        // which would tie this test to other tests that may have
        // already populated the same slot in this process.
        use super::probe;
        use crate::types::PackageManager;

        let first = probe(PackageManager::Composer);
        let second = probe(PackageManager::Composer);
        assert_eq!(first, second, "repeat probes must observe same value");
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
