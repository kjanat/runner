//! Platform-specific argv for script files, without a shell command string.

use std::ffi::OsString;
use std::path::Path;

use crate::Shebang;

pub(crate) fn shebang_argv(
    shebang: &Shebang,
    file: &Path,
    cwd: &Path,
    args: &[String],
) -> Vec<OsString> {
    argv_for_platform(shebang, file, cwd, args, cfg!(windows))
}

fn argv_for_platform(
    shebang: &Shebang,
    file: &Path,
    cwd: &Path,
    args: &[String],
    windows: bool,
) -> Vec<OsString> {
    let name = shebang
        .program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&shebang.program);
    let lower = name.to_ascii_lowercase();
    let posix = windows
        && matches!(
            lower.trim_end_matches(".exe"),
            "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish"
        );
    let program = if posix && !Path::new(&shebang.program).is_file() {
        name
    } else {
        &shebang.program
    };
    let file = if posix {
        windows_shell_path(&file.to_string_lossy(), &cwd.to_string_lossy()).into()
    } else {
        file.as_os_str().to_owned()
    };
    std::iter::once(OsString::from(program))
        .chain(shebang.args.iter().map(OsString::from))
        .chain(std::iter::once(file))
        .chain(args.iter().map(OsString::from))
        .collect()
}

fn windows_shell_path(file: &str, cwd: &str) -> String {
    let file = file.replace('\\', "/");
    let prefix = format!("{}/", cwd.replace('\\', "/").trim_end_matches('/'));
    let relative = file
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(&prefix));
    let text = if relative {
        &file[prefix.len()..]
    } else {
        &file
    };
    match text.as_bytes() {
        [drive, b':', b'/', rest @ ..] if drive.is_ascii_alphabetic() => format!(
            "/{}/{}",
            char::from(drive.to_ascii_lowercase()),
            String::from_utf8_lossy(rest)
        ),
        [b'-', ..] => format!("./{text}"),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_posix_script_argv_preserves_paths_and_argument_boundaries() {
        for (program, file, cwd, expected_program, expected_file) in [
            (
                "/runner-test-nonexistent/bin/bash",
                r"C:\repo\scripts\task.sh",
                r"C:\repo",
                "bash",
                "scripts/task.sh",
            ),
            (
                "/runner-test-nonexistent/bin/sh",
                r"D:\other dir\task.sh",
                r"C:\repo",
                "sh",
                "/d/other dir/task.sh",
            ),
            (
                "BASH.EXE",
                r"C:\repo\-task.sh",
                r"C:\repo",
                "BASH.EXE",
                "./-task.sh",
            ),
            (
                "/runner-test-nonexistent/bin/zsh",
                r"C:\repository\task.sh",
                r"C:\repo",
                "zsh",
                "/c/repository/task.sh",
            ),
        ] {
            let shebang = Shebang {
                program: program.into(),
                args: vec!["-e".into()],
            };
            let words = argv_for_platform(
                &shebang,
                Path::new(file),
                Path::new(cwd),
                &["arg with spaces".into()],
                true,
            );
            assert_eq!(
                words,
                [expected_program, "-e", expected_file, "arg with spaces"].map(OsString::from)
            );
        }
    }

    #[test]
    fn unix_and_non_shell_interpreters_keep_native_paths() {
        for (program, windows) in [("/bin/bash", false), ("python3", true)] {
            let shebang = Shebang {
                program: program.into(),
                args: vec![],
            };
            let file = Path::new(r"C:\repo\task");
            assert_eq!(
                argv_for_platform(&shebang, file, Path::new(r"C:\repo"), &[], windows),
                vec![OsString::from(program), file.as_os_str().to_owned()]
            );
        }
    }
}
