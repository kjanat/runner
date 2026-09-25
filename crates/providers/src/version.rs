//! Host executable version observations.

/// Read the first nonempty version line from the provider's host executable,
/// queried from `dir` so a directory-aware shim answers for that project.
///
/// # Errors
/// Returns the provider's failed query or unreadable output.
pub(crate) fn read(
    dir: &std::path::Path,
    present: &runner_core::Present,
) -> Result<String, runner_core::Warning> {
    let provider = crate::REGISTRY.by_id(present.provider);
    let error = |message| runner_core::Warning::about(provider.id, message);
    let program = provider
        .program
        .ok_or_else(|| error("no executable to query".to_owned()))?;
    let program = runner_core::probe_with(program, &present.bin_dirs)
        .ok_or_else(|| error(format!("{} is not on PATH", provider.label)))?;
    let output = std::process::Command::new(program)
        .arg("--version")
        .current_dir(dir)
        .output()
        .map_err(|e| error(e.to_string()))?;
    if !output.status.success() {
        return Err(error(format!(
            "{} --version failed ({})",
            provider.label, output.status
        )));
    }
    let text = String::from_utf8(output.stdout).map_err(|e| error(e.to_string()))?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| error(format!("{} returned no version", provider.label)))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn the_query_runs_in_the_given_directory_through_the_present_bin_dirs() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("runner-version-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        let project = dir.join("project");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let yarn = bin.join("yarn");
        std::fs::write(&yarn, "#!/bin/sh\npwd -P\n").unwrap();
        std::fs::set_permissions(&yarn, std::fs::Permissions::from_mode(0o755)).unwrap();
        let present = runner_core::Present {
            provider: runner_core::ProviderId::Yarn,
            scope: runner_core::Scope::Root,
            version: None,
            bin_dirs: vec![bin],
            because: Vec::new(),
        };
        let reported = super::read(&project, &present).unwrap();
        assert_eq!(
            std::path::PathBuf::from(reported).canonicalize().unwrap(),
            project.canonicalize().unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
