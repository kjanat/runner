//! Spawn external programs with Windows-aware PATH resolution.
//!
//! Rust's `Command::new("npm")` calls `CreateProcessW` on Windows, which does
//! not consult `PATHEXT`. Tools like npm, yarn, and pnpm ship as `.cmd` shims,
//! so a bare-name spawn fails with "program not found" even though `npm` runs
//! fine from PowerShell. Resolve the name against `PATH` × `PATHEXT` here so
//! every tool module can stay platform-agnostic.

use std::process::Command;

/// Fallback when `PATHEXT` is unset: the stock Windows value (from
/// `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`),
/// so resolution matches what cmd.exe would do on a vanilla host. Shared
/// with the bin-dir re-resolution in `cmd::configure_command` so the two
/// fallbacks cannot drift.
#[cfg(windows)]
pub(crate) const DEFAULT_PATHEXT: &str =
    ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC;.CPL";

/// Build a [`Command`] for `name`, resolving Windows shims via `PATHEXT`.
///
/// On non-Windows targets this is a thin wrapper around [`Command::new`]. On
/// Windows, when `name` is a bare program name (no path separator), walks
/// `PATH` × `PATHEXT` and returns a [`Command`] whose program is the first
/// matching file. If nothing matches, falls back to [`Command::new`] so the
/// underlying spawn error is still surfaced unchanged.
pub(crate) fn command(name: &str) -> Command {
    #[cfg(windows)]
    {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let pathext = std::env::var_os("PATHEXT").unwrap_or_else(|| DEFAULT_PATHEXT.into());
        if let Some(resolved) = resolve_windows(name, &path, &pathext) {
            return Command::new(resolved);
        }
    }
    Command::new(name)
}

/// Walk `path` × `pathext` looking for an executable for `name`.
///
/// Pure function, takes the relevant env vars as arguments so tests can drive
/// it on any host. Returns `None` when `name` already contains a path
/// separator (`CreateProcessW` handles those directly) or no candidate matches.
#[cfg(any(windows, test))]
pub(crate) fn resolve_windows(
    name: &str,
    path: &std::ffi::OsStr,
    pathext: &std::ffi::OsStr,
) -> Option<std::path::PathBuf> {
    use std::path::Path;

    if name.is_empty() || Path::new(name).components().count() > 1 {
        return None;
    }

    let pathext = pathext.to_string_lossy();
    let exts: Vec<&str> = pathext.split(';').filter(|ext| !ext.is_empty()).collect();

    // Matches cmd.exe: when `name` carries any extension, the exact spelling is
    // used and PATHEXT is not consulted. Only fall back to PATHEXT for bare
    // names with no extension at all.
    let has_explicit_extension = Path::new(name).extension().is_some();

    for dir in std::env::split_paths(path) {
        if has_explicit_extension {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    use super::{command, resolve_windows};
    use crate::tool::test_support::TempDir;

    #[test]
    fn command_passthrough_when_unresolved() {
        // On non-Windows hosts this is the only path. On Windows it exercises
        // the miss-fallback (bare name unchanged when nothing on PATH matches).
        let cmd = command("definitely-not-on-path-xyz");
        assert_eq!(cmd.get_program(), "definitely-not-on-path-xyz");
    }

    #[test]
    fn resolve_finds_cmd_shim_via_pathext() {
        let dir = TempDir::new("program-resolve");
        let shim = dir.path().join("foo.cmd");
        fs::write(&shim, "@echo off\n").expect("shim should be written");

        let path = OsString::from(dir.path());
        let pathext = OsString::from(".com;.exe;.bat;.cmd");

        let resolved = resolve_windows("foo", &path, &pathext);

        assert_eq!(resolved, Some(shim));
    }

    #[test]
    fn resolve_prefers_earlier_path_entry() {
        let first = TempDir::new("program-first");
        let second = TempDir::new("program-second");
        let preferred = first.path().join("foo.cmd");
        let fallback = second.path().join("foo.cmd");
        fs::write(&preferred, "@echo off\n").expect("first shim should be written");
        fs::write(&fallback, "@echo off\n").expect("second shim should be written");

        let mut joined = OsString::from(first.path());
        joined.push(if cfg!(windows) { ";" } else { ":" });
        joined.push(second.path());
        let pathext = OsString::from(".cmd");

        let resolved = resolve_windows("foo", &joined, &pathext);

        assert_eq!(resolved, Some(preferred));
    }

    #[test]
    fn resolve_returns_none_when_missing() {
        let dir = TempDir::new("program-miss");

        let path = OsString::from(dir.path());
        let pathext = OsString::from(".com;.exe;.bat;.cmd");

        let resolved = resolve_windows("ghost", &path, &pathext);

        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_skips_names_with_separators() {
        let dir = TempDir::new("program-abs");
        let path = OsString::from(dir.path());
        let pathext = OsString::from(".cmd");

        let with_sep = if cfg!(windows) {
            r"C:\Windows\System32\where.exe"
        } else {
            "/usr/bin/env"
        };

        assert_eq!(resolve_windows(with_sep, &path, &pathext), None);
    }

    #[test]
    fn resolve_matches_name_with_explicit_extension() {
        let dir = TempDir::new("program-explicit-ext");
        let shim = dir.path().join("foo.cmd");
        fs::write(&shim, "@echo off\n").expect("shim should be written");

        let path = OsString::from(dir.path());
        let pathext = OsString::from(".cmd");

        let resolved = resolve_windows("foo.cmd", &path, &pathext);

        assert_eq!(resolved, Some(shim));
    }

    // An explicit extension must short-circuit PATHEXT; a later directory
    // holding the exact name wins over an earlier `<name><ext>` candidate. This
    // is the cmd.exe behaviour and avoids resolving `foo.cmd.exe` for `foo.cmd`.
    #[test]
    fn resolve_explicit_extension_skips_pathext() {
        let first = TempDir::new("program-explicit-first");
        let second = TempDir::new("program-explicit-second");
        let decoy = first.path().join("foo.cmd.exe");
        let exact = second.path().join("foo.cmd");
        fs::write(&decoy, "@echo off\n").expect("decoy should be written");
        fs::write(&exact, "@echo off\n").expect("exact match should be written");

        let mut joined = OsString::from(first.path());
        joined.push(if cfg!(windows) { ";" } else { ":" });
        joined.push(second.path());
        let pathext = OsString::from(".exe;.cmd");

        let resolved = resolve_windows("foo.cmd", &joined, &pathext);

        assert_eq!(resolved, Some(exact));
    }

    #[test]
    fn resolve_handles_empty_name() {
        let pathext = OsString::from(".cmd");
        let path = OsString::new();
        assert_eq!(resolve_windows("", &path, &pathext), None);
    }

    // Sanity: the helper produces a `PathBuf` whose program survives a
    // `Command::new` round-trip. Guards against future refactors that might
    // mangle the resolved value.
    #[test]
    fn resolved_path_round_trips_through_command() {
        let dir = TempDir::new("program-roundtrip");
        let shim = dir.path().join("foo.cmd");
        fs::write(&shim, "@echo off\n").expect("shim should be written");

        let path = OsString::from(dir.path());
        let pathext = OsString::from(".cmd");
        let resolved = resolve_windows("foo", &path, &pathext).expect("foo.cmd should resolve");

        let cmd = std::process::Command::new(&resolved);
        assert_eq!(PathBuf::from(cmd.get_program()), shim);
    }
}
