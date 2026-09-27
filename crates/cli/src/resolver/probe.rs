//! `PATH` probing shared by the doctor's signal report.

use std::path::PathBuf;

/// [`runner_core::probe_in`], the search every `PATH` probe shares.
pub(crate) fn probe_in(
    name: &str,
    path: &std::ffi::OsStr,
    pathext: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    runner_core::probe_in(name, path, pathext)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::probe_in;
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
