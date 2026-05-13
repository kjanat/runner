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
//! [`probe`] memoizes results in a process-wide `LazyLock<Mutex<HashMap>>`
//! keyed by the binary name. Each resolver invocation hits PATH at most
//! once per unique name; subsequent lookups return the cached
//! `Option<PathBuf>`. This is the "one-shot per session" cache called
//! out in the plan — `probe_all(NODE_PROBE_ORDER)` ends up walking PATH
//! four times in the worst case, all of which are then memoized for
//! later `apply_manifest_on_fail` / `--explain` calls.
//!
//! The pure-function variant [`probe_in`] stays cache-free so tests
//! exercise the search logic against a controlled directory without
//! racing or polluting the shared cache.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use crate::types::PackageManager;

/// Process-wide memoization of [`probe`] lookups.
///
/// Bounded by the set of unique binary names the resolver ever probes —
/// at most a few dozen — so leaking nothing on shutdown is fine.
/// `Mutex` over `RwLock` here because writes happen on first probe of
/// every distinct name (lots of cold inserts) while reads are cheap and
/// short; the wait under contention is bounded by a single hash lookup.
static CACHE: LazyLock<Mutex<HashMap<String, Option<PathBuf>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Probe `$PATH` for `name`. Returns the absolute path of the first
/// matching executable, or `None` if nothing is found.
///
/// On Windows, also walks `PATHEXT` so that `cmd`/`bat` shims are found —
/// the same approach used by [`crate::tool::program::command`].
///
/// Result is memoized in [`CACHE`] for the lifetime of the process; the
/// pure-function variant [`probe_in`] is exempt for testability.
pub(crate) fn probe(name: &str) -> Option<PathBuf> {
    {
        let cache = CACHE.lock().expect("probe cache poisoned");
        if let Some(cached) = cache.get(name) {
            return cached.clone();
        }
    }
    let result = std::env::var_os("PATH")
        .and_then(|path| probe_in(name, &path, std::env::var_os("PATHEXT").as_deref()));
    CACHE
        .lock()
        .expect("probe cache poisoned")
        .insert(name.to_string(), result.clone());
    result
}

/// Clear the [`probe`] cache. Test-only — production code relies on the
/// cache living for the full process lifetime so the resolver never
/// pays for repeat PATH walks.
#[cfg(test)]
pub(crate) fn clear_cache_for_testing() {
    CACHE.lock().expect("probe cache poisoned").clear();
}

/// Inspect the [`probe`] cache size. Test-only — used to verify that
/// repeat lookups of the same name hit the cache instead of re-walking
/// PATH.
#[cfg(test)]
pub(crate) fn cache_len_for_testing() -> usize {
    CACHE.lock().expect("probe cache poisoned").len()
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
        .filter_map(|&pm| probe(pm.label()).map(|path| (pm, path)))
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
    fn probe_memoizes_lookups_per_name() {
        use super::{cache_len_for_testing, clear_cache_for_testing, probe};

        // SAFETY: the test mutates the shared cache and shouldn't race
        // with other tests touching the same names. We pick a
        // deliberately unlikely binary name so a real install can't
        // shadow the lookup, and snapshot the cache state around our
        // calls.
        clear_cache_for_testing();
        let starting_len = cache_len_for_testing();

        let unlikely = "runner-cache-test-bin-zzzqqqq";
        let first = probe(unlikely);
        let after_first = cache_len_for_testing();
        let second = probe(unlikely);
        let after_second = cache_len_for_testing();

        assert_eq!(first, second, "cached lookup should match first call");
        assert_eq!(
            after_first,
            starting_len + 1,
            "first probe should insert one cache entry"
        );
        assert_eq!(
            after_second, after_first,
            "second probe must hit the cache, not re-walk PATH",
        );

        // Don't leave the test entry hanging around for other tests
        // that snapshot cache len.
        clear_cache_for_testing();
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
