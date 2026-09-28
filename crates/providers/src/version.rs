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
    let program = runner_core::probe_with(program, &[])
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
    fn write_executable(path: &std::path::Path, contents: &str) {
        use std::io::Write as _;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("sh writes the executable");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(contents.as_bytes())
            .expect("contents written");
        assert!(child.wait().expect("sh exits").success());
    }

    #[cfg(unix)]
    #[test]
    fn the_query_never_runs_an_executable_from_the_present_bin_dirs() {
        let dir = std::env::temp_dir().join(format!("runner-version-bins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        let project = dir.join("project");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        write_executable(
            &bin.join("yarn"),
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran\"\necho 9.9.9\n",
        );
        let present = runner_core::Present {
            provider: runner_core::ProviderId::Yarn,
            scope: runner_core::Scope::Root,
            version: None,
            bin_dirs: vec![bin.clone()],
            because: Vec::new(),
        };
        let reported = super::read(&project, &present);
        assert!(!bin.join("ran").exists(), "{reported:?}");
        assert_ne!(reported.ok().as_deref(), Some("9.9.9"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
