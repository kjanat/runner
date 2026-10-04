//! `PATH`-style directory lists.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;

/// The directories in `list`.
#[cfg(any(unix, windows, target_os = "uefi"))]
#[must_use]
pub fn split(list: &OsStr) -> Vec<PathBuf> {
    std::env::split_paths(list).collect()
}

/// The directories in `list`.
#[cfg(all(target_os = "wasi", target_env = "p1"))]
#[must_use]
pub fn split(list: &OsStr) -> Vec<PathBuf> {
    use std::os::wasi::ffi::OsStrExt;

    list.as_bytes()
        .split(|byte| *byte == b':')
        .map(|dir| PathBuf::from(OsStr::from_bytes(dir)))
        .collect()
}

/// The directories in `list`, read as UTF-8.
#[cfg(not(any(
    unix,
    windows,
    target_os = "uefi",
    all(target_os = "wasi", target_env = "p1")
)))]
#[must_use]
pub fn split(list: &OsStr) -> Vec<PathBuf> {
    list.to_string_lossy()
        .split(':')
        .map(PathBuf::from)
        .collect()
}

/// `dirs` as one list.
///
/// # Errors
///
/// When a directory contains the list separator.
#[cfg(any(unix, windows, target_os = "uefi"))]
pub fn join(dirs: impl IntoIterator<Item = PathBuf>) -> io::Result<OsString> {
    std::env::join_paths(dirs).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

/// `dirs` as one list.
///
/// # Errors
///
/// When a directory contains the list separator.
#[cfg(not(any(unix, windows, target_os = "uefi")))]
pub fn join(dirs: impl IntoIterator<Item = PathBuf>) -> io::Result<OsString> {
    let mut joined = OsString::new();
    for (index, dir) in dirs.into_iter().enumerate() {
        if dir.as_os_str().as_encoded_bytes().contains(&b':') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path segment contains separator `:`",
            ));
        }
        if index > 0 {
            joined.push(":");
        }
        joined.push(dir);
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{join, split};

    #[test]
    fn join_then_split_round_trips() {
        let dirs = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(split(&join(dirs.clone()).expect("joins")), dirs);
    }

    #[cfg(all(target_os = "wasi", target_env = "p1"))]
    #[test]
    fn non_utf8_dirs_round_trip() {
        use std::ffi::OsStr;
        use std::os::wasi::ffi::OsStrExt;

        let dirs = vec![
            PathBuf::from(OsStr::from_bytes(b"a\xff")),
            PathBuf::from("b"),
        ];
        assert_eq!(split(&join(dirs.clone()).expect("joins")), dirs);
    }
}
