//! Volta toolchain manager — shim classification and `volta which`
//! resolution.
//!
//! Volta interposes shims for `node`/`npm`/`yarn`/`pnpm` on `PATH`
//! (Windows: next to `volta.exe`, e.g. `C:\Program Files\Volta\`;
//! Unix: `~/.volta/bin`). A shim exists even when the tool it fronts
//! was never provisioned, so a raw PATH probe can report a binary that
//! cannot actually run. This module classifies probe hits as shims and
//! resolves them to the real provisioned binary via `volta which`.
//!
//! Display/diagnostics only: execution always spawns the shim itself,
//! because the shim performs Volta's per-project version selection.

use std::path::{Path, PathBuf};

use crate::tool::program;

/// A located Volta installation reduced to its shim directories.
#[derive(Debug, Clone)]
pub(crate) struct VoltaInstall {
    /// Canonicalized directories whose executables are Volta shims.
    shim_dirs: Vec<PathBuf>,
}

impl VoltaInstall {
    /// Locate Volta from the live environment: the directory holding
    /// the `volta` binary on `PATH`, plus `$VOLTA_HOME/bin` when set.
    /// Returns `None` when neither signal exists — no Volta, nothing
    /// to classify.
    pub(crate) fn locate() -> Option<Self> {
        let volta_bin = std::env::var_os("PATH").and_then(|path| {
            crate::resolver::probe_path_for_doctor(
                "volta",
                &path,
                std::env::var_os("PATHEXT").as_deref(),
            )
        });
        let volta_home = std::env::var_os("VOLTA_HOME").map(PathBuf::from);
        Self::from_candidates(volta_bin.as_deref(), volta_home.as_deref())
    }

    /// Pure constructor for tests — injected candidates, no env reads.
    pub(crate) fn from_candidates(
        volta_bin: Option<&Path>,
        volta_home: Option<&Path>,
    ) -> Option<Self> {
        let mut shim_dirs = Vec::new();
        if let Some(parent) = volta_bin.and_then(Path::parent) {
            shim_dirs.push(canonical_dir(parent));
        }
        if let Some(home) = volta_home {
            shim_dirs.push(canonical_dir(&home.join("bin")));
        }
        shim_dirs.dedup();
        if shim_dirs.is_empty() {
            None
        } else {
            Some(Self { shim_dirs })
        }
    }

    /// True when `bin` lives directly in one of the shim directories.
    /// Exact parent-directory equality, not prefix matching —
    /// `<shimdir>/nested/npm` is not a shim. Only the *parent* is
    /// canonicalized: on Unix the shims themselves are symlinks to
    /// `volta-shim`, and canonicalizing the file would escape the bin
    /// directory entirely.
    pub(crate) fn is_shim(&self, bin: &Path) -> bool {
        let Some(parent) = bin.parent() else {
            return false;
        };
        let canonical = canonical_dir(parent);
        self.shim_dirs.contains(&canonical)
    }
}

/// Canonicalize a directory for comparison (resolves `\\?\` prefixes
/// and on-disk casing on Windows); fall back to the lexical path when
/// canonicalization fails so comparison degrades instead of erroring.
fn canonical_dir(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// Outcome of resolving one shim through `volta which`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShimResolution {
    /// The tool is provisioned; this is the real binary the shim runs.
    Resolved(PathBuf),
    /// Volta answered but has no version of the tool ("No default …").
    NotProvisioned,
    /// Volta itself failed to answer (spawn error, empty output) —
    /// claim nothing.
    Unknown,
}

/// Ask `volta which <tool>` for the real provisioned binary.
///
/// Runs with `project_root` as the working directory because `volta
/// which` honors the project pinning of its CWD. Classification uses
/// exit status and stdout only — Volta's error wording ("No default
/// npm version set") varies across versions and must not be parsed.
pub(crate) fn resolve_shim(tool: &str, project_root: &Path) -> ShimResolution {
    match program::command("volta")
        .args(["which", tool])
        .current_dir(project_root)
        .output()
    {
        Ok(out) => classify_which_output(out.status.success(), &out.stdout),
        Err(_) => ShimResolution::Unknown,
    }
}

/// Pure decoder for `volta which` output, split out so tests don't
/// need a Volta installation.
fn classify_which_output(success: bool, stdout: &[u8]) -> ShimResolution {
    if !success {
        return ShimResolution::NotProvisioned;
    }
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        ShimResolution::Unknown
    } else {
        ShimResolution::Resolved(PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{ShimResolution, VoltaInstall, classify_which_output};
    use crate::tool::test_support::TempDir;

    #[test]
    fn from_candidates_uses_parent_of_volta_bin() {
        let dir = TempDir::new("volta-bin-parent");
        let volta = dir.path().join("volta.exe");
        let npm = dir.path().join("npm.exe");
        fs::write(&volta, "").expect("write volta stub");
        fs::write(&npm, "").expect("write npm stub");

        let install =
            VoltaInstall::from_candidates(Some(&volta), None).expect("volta bin is evidence");
        assert!(
            install.is_shim(&npm),
            "sibling of volta must classify as shim"
        );
        assert!(
            !install.is_shim(Path::new("/somewhere/else/npm")),
            "unrelated dirs must not classify"
        );
    }

    #[test]
    fn from_candidates_adds_volta_home_bin() {
        let home = TempDir::new("volta-home");
        let bin = home.path().join("bin");
        fs::create_dir_all(&bin).expect("create bin dir");
        let yarn = bin.join("yarn");
        fs::write(&yarn, "").expect("write yarn stub");

        let install =
            VoltaInstall::from_candidates(None, Some(home.path())).expect("VOLTA_HOME is evidence");
        assert!(install.is_shim(&yarn));
    }

    #[test]
    fn from_candidates_none_without_evidence() {
        assert!(VoltaInstall::from_candidates(None, None).is_none());
    }

    #[test]
    fn is_shim_requires_exact_parent_dir() {
        let dir = TempDir::new("volta-exact-parent");
        let volta = dir.path().join("volta");
        fs::write(&volta, "").expect("write volta stub");
        let nested_dir = dir.path().join("nested");
        fs::create_dir_all(&nested_dir).expect("create nested dir");
        let nested = nested_dir.join("npm");
        fs::write(&nested, "").expect("write nested stub");

        let install =
            VoltaInstall::from_candidates(Some(&volta), None).expect("volta bin is evidence");
        assert!(!install.is_shim(&nested), "no prefix matching: {nested:?}");
    }

    #[test]
    fn classify_which_output_resolves_trimmed_path() {
        let resolved = classify_which_output(true, b"C:\\Volta\\image\\npm\\11.6.2\\npm.cmd\r\n");
        assert_eq!(
            resolved,
            ShimResolution::Resolved(PathBuf::from("C:\\Volta\\image\\npm\\11.6.2\\npm.cmd")),
        );
    }

    #[test]
    fn classify_which_output_nonzero_is_not_provisioned() {
        assert_eq!(
            classify_which_output(false, b""),
            ShimResolution::NotProvisioned,
        );
    }

    #[test]
    fn classify_which_output_empty_stdout_is_unknown() {
        assert_eq!(
            classify_which_output(true, b"  \n"),
            ShimResolution::Unknown
        );
    }

    /// Availability-gated smoke test: only meaningful on a host with
    /// Volta installed; skips (with a note) elsewhere, mirroring the
    /// `just`-gated integration tests.
    #[test]
    fn volta_which_smoke() {
        let Some(path) = std::env::var_os("PATH") else {
            eprintln!("skipping: no PATH");
            return;
        };
        if crate::resolver::probe_path_for_doctor(
            "volta",
            &path,
            std::env::var_os("PATHEXT").as_deref(),
        )
        .is_none()
        {
            eprintln!("skipping: `volta` not found on PATH");
            return;
        }
        let cwd = std::env::current_dir().expect("cwd exists");
        // Any variant is acceptable; the assertion is "does not panic
        // and answers something classifiable".
        let _ = super::resolve_shim("node", &cwd);
    }
}
