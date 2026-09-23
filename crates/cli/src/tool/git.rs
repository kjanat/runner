//! Git queries used by detection.

use std::path::Path;
use std::process::Stdio;

/// Which of `candidates` git tracks in `dir`, as paths relative to `dir`.
///
/// `None` means git had no answer at all: not installed, not a repository, or
/// the command failed. That is a different fact from an empty list (git
/// answered, and tracks none of them), and callers depend on the difference.
pub(crate) fn tracked(dir: &Path, candidates: &[&str]) -> Option<Vec<String>> {
    if candidates.is_empty() {
        return Some(Vec::new());
    }
    let output = super::program::command("git")
        .arg("ls-files")
        .arg("-z")
        .arg("--")
        .args(candidates)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::tracked;
    use crate::tool::test_support::TempDir;

    fn git(dir: &std::path::Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn reports_only_the_committed_file() {
        let dir = TempDir::new("git-tracked");
        if !git(dir.path(), &["init"]) {
            eprintln!("skipping: git unavailable");
            return;
        }
        std::fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        std::fs::write(dir.path().join("package-lock.json"), "").expect("package-lock.json");
        std::fs::write(dir.path().join(".gitignore"), "package-lock.json\n").expect(".gitignore");
        assert!(git(dir.path(), &["add", "bun.lock", ".gitignore"]));
        assert!(git(dir.path(), &["commit", "-m", "lock"]));

        let tracked = tracked(dir.path(), &["bun.lock", "package-lock.json"])
            .expect("git answers inside a repository");
        assert_eq!(tracked, vec!["bun.lock".to_string()]);
    }

    #[test]
    fn no_answer_outside_a_repository() {
        let dir = TempDir::new("git-untracked");
        std::fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        assert!(
            tracked(dir.path(), &["bun.lock"]).is_none(),
            "no repository is not the same as nothing tracked",
        );
    }
}
