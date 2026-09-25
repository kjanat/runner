//! Host executable version observations.

/// Read the first nonempty version line from the provider's host executable.
///
/// # Errors
/// Returns the provider's failed query or unreadable output.
pub(crate) fn read(present: &runner_core::Present) -> Result<String, runner_core::Warning> {
    let provider = crate::REGISTRY.by_id(present.provider);
    let error = |message| runner_core::Warning::about(provider.id, message);
    let program = provider
        .program
        .ok_or_else(|| error("no executable to query".to_owned()))?;
    let program = runner_core::probe_with(program, &[])
        .ok_or_else(|| error(format!("{} is not on PATH", provider.label)))?;
    let output = std::process::Command::new(program)
        .arg("--version")
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
