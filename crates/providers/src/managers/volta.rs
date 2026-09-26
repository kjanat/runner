//! Volta.

use std::path::{Path, PathBuf};

use runner_core::{
    Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, Shim, ShimsCap, Signal,
};

/// Volta.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Volta,
    label: "volta",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::TOOL_MANAGER,
    program: Some("volta"),
    signals: &[Signal::EnvVar("VOLTA_HOME"), Signal::Probe("volta")],
    caps: Capabilities {
        shims: Some(ShimsCap {
            dirs: shim_dirs,
            resolve,
        }),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};

/// The directory holding the `volta` on `PATH`, plus `$VOLTA_HOME/bin`.
fn shim_dirs() -> Vec<PathBuf> {
    dirs_from(
        runner_core::probe_with("volta", &[]).as_deref(),
        std::env::var_os("VOLTA_HOME").map(PathBuf::from).as_deref(),
    )
}

fn dirs_from(volta: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = volta
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .into_iter()
        .chain(home.map(|home| home.join("bin")))
        .map(|dir| dir.canonicalize().unwrap_or(dir))
        .collect();
    dirs.dedup();
    dirs
}

/// Ask `volta which <tool>` from `dir`, whose project pinning Volta honours.
/// Volta's error wording varies across versions, so only the exit status and
/// stdout are read.
fn resolve(tool: &str, dir: &Path) -> Shim {
    let Some(volta) = runner_core::probe_with("volta", &[]) else {
        return Shim::Unknown;
    };
    match std::process::Command::new(volta)
        .args(["which", tool])
        .current_dir(dir)
        .output()
    {
        Ok(out) => classify(out.status.success(), &out.stdout),
        Err(_) => Shim::Unknown,
    }
}

fn classify(success: bool, stdout: &[u8]) -> Shim {
    if !success {
        return Shim::NotProvisioned;
    }
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        Shim::Unknown
    } else {
        Shim::Resolved(PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use runner_core::Shim;

    use super::{classify, dirs_from};
    use crate::extract::test_support::TempDir;

    #[test]
    fn the_shim_dirs_are_the_volta_parent_and_volta_home_bin() {
        let dir = TempDir::new("volta-dirs");
        let volta = dir.path().join("volta.exe");
        std::fs::write(&volta, "").expect("volta stub");
        let home = TempDir::new("volta-home");
        std::fs::create_dir_all(home.path().join("bin")).expect("bin dir");

        let dirs = dirs_from(Some(&volta), Some(home.path()));
        assert_eq!(
            dirs,
            [
                dir.path().canonicalize().expect("canonical"),
                home.path().join("bin").canonicalize().expect("canonical"),
            ]
        );
        assert_eq!(dirs_from(None, None), Vec::<PathBuf>::new());
    }

    #[test]
    fn volta_which_output_classifies_by_status_and_stdout() {
        assert_eq!(
            classify(true, b"C:\\Volta\\image\\npm\\11.6.2\\npm.cmd\r\n"),
            Shim::Resolved(PathBuf::from("C:\\Volta\\image\\npm\\11.6.2\\npm.cmd")),
        );
        assert_eq!(classify(false, b""), Shim::NotProvisioned);
        assert_eq!(classify(true, b"  \n"), Shim::Unknown);
    }
}
