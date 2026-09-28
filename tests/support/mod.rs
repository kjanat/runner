//! Spawning the binaries under test in the same environment on every host.

use std::ffi::OsStr;
use std::process::Command;

use actions_rs::env::vars;

/// Variables that switch runner into CI or GitHub Actions behaviour.
pub(crate) const CI_VARIABLES: [&str; 2] = [vars::CI, vars::GITHUB_ACTIONS];

/// `program` with every `RUNNER_*`, `GITHUB_*` and CI variable removed, so a
/// test sees the same behaviour locally and under GitHub Actions. A test that
/// wants CI behaviour sets the variable itself.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for name in CI_VARIABLES {
        command.env_remove(name);
    }
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("RUNNER_") || upper.starts_with("GITHUB_") {
            command.env_remove(&key);
        }
    }
    command
}
