//! `PATH` probe, step 7 of the resolution chain.
//!
//! When no manifest, lockfile, or override signal points the resolver at a
//! package manager, this module walks `$PATH` (and `PATHEXT` on Windows)
//! to discover what is actually installed. The Node ecosystem returns the
//! first match in canonical order, `npm > bun > pnpm > yarn`: a bare
//! `package.json` is an npm project, and an alternate manager is picked
//! only when npm itself is missing from `$PATH`.
//!
//! ## Caching
//!
//! [`probe`] memoizes per-PM in a static `OnceLock` array, giving
//! exactly-once probing across concurrent callers without holding a lock
//! during the PATH walk. The pure-function variant [`probe_in`] stays
//! cache-free so tests can exercise the search against a controlled
//! directory without racing or polluting the shared cache.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::types::PackageManager;

/// Process-wide cache of [`probe`] lookups, one slot per
/// [`PackageManager`] variant. `OnceLock` initialises lazily and
/// guarantees the initialiser runs at most once even when called
/// concurrently, exactly the semantics we want here.
static CACHE: [OnceLock<Option<PathBuf>>; PackageManager::COUNT] =
    [const { OnceLock::new() }; PackageManager::COUNT];

/// Probe `$PATH` for `pm`. Returns the absolute path of the first
/// matching executable, or `None` if nothing is found.
///
/// On Windows, also walks `PATHEXT` so that `cmd`/`bat` shims are found,
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

/// [`runner_core::probe_in`], the search every `PATH` probe shares.
pub(crate) fn probe_in(
    name: &str,
    path: &std::ffi::OsStr,
    pathext: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    runner_core::probe_in(name, path, pathext)
}

/// The package managers that dispatch `package.json` scripts, in the order
/// the core probes `PATH` for one.
pub(crate) fn node_probe_order() -> Vec<PackageManager> {
    let mut providers: Vec<_> = runner_providers::REGISTRY
        .iter()
        .filter(|provider| {
            provider.kind.contains(runner_core::Kind::PACKAGE_MANAGER)
                && provider
                    .caps
                    .run_task
                    .is_some_and(|cap| cap.sources.contains(&runner_core::ProviderId::PackageJson))
        })
        .collect();
    providers.sort_by_key(|provider| provider.caps.probe_priority);
    providers
        .into_iter()
        .filter_map(|provider| PackageManager::from_label(provider.label))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::{node_probe_order, probe_in};
    use crate::tool::test_support::TempDir;

    #[test]
    fn extra_dirs_are_searched_ahead_of_path() {
        // A tool manager installs without activating, so its package
        // manager is real but absent from `$PATH`. Without the extra
        // directories the probe reports nothing and the install becomes a
        // silent no-op.
        let dir = TempDir::new("probe-extra");
        let tool_bin = dir.path().join("mise-installs").join("node").join("bin");
        fs::create_dir_all(&tool_bin).expect("tool bin dir should be created");
        let npm = tool_bin.join(if cfg!(windows) { "npm.exe" } else { "npm" });
        fs::write(&npm, "#!/bin/sh\n").expect("shim should be written");
        executable(&npm);

        // An empty PATH stands in for a shell that never ran the hook.
        let empty = OsString::new();
        assert!(
            probe_in("npm", &empty, None).is_none(),
            "nothing on PATH means nothing to find",
        );

        let search = std::env::join_paths([tool_bin]).expect("joins");
        assert_eq!(
            probe_in("npm", &search, None),
            Some(npm),
            "the tool manager's bin dir must be searched",
        );
    }

    #[test]
    fn probe_in_finds_executable_by_bare_name() {
        let dir = TempDir::new("probe-bare");
        let target = dir
            .path()
            .join(if cfg!(windows) { "pnpm.exe" } else { "pnpm" });
        fs::write(&target, "#!/bin/sh\n").expect("shim should be written");
        executable(&target);

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
        executable(&shim);

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
        // Asserts the caller-visible property, repeated `probe(pm)`
        // calls return the same cached value, rather than poking at
        // cache internals, which other tests in this process may have
        // already populated.
        use super::probe;
        use crate::types::PackageManager;

        let first = probe(PackageManager::Composer);
        let second = probe(PackageManager::Composer);
        assert_eq!(first, second, "repeat probes must observe same value");
    }

    #[test]
    fn node_probe_order_is_npm_first() {
        assert_eq!(
            node_probe_order(),
            [
                crate::types::PackageManager::Npm,
                crate::types::PackageManager::Bun,
                crate::types::PackageManager::Pnpm,
                crate::types::PackageManager::Yarn,
                crate::types::PackageManager::Deno,
            ]
        );
    }

    fn executable(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                .expect("shim should be executable");
        }
        #[cfg(not(unix))]
        let _ = path;
    }
}
