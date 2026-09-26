//! What a provider's health check reports.

/// The parsed result of a provider's health command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Nothing to report.
    Ok,
    /// The tool's own messages, one per problem.
    Problems(Vec<String>),
    /// The output could not be read.
    Unreadable(String),
}

/// Execute one declared health check using a host-trusted plan.
///
/// # Errors
/// Returns planning, spawn, or unreadable-output errors.
pub fn check(
    tree: &crate::Tree,
    project: &crate::Project,
    policy: &crate::Policy,
    present: &crate::Present,
    index: usize,
    registry: &crate::Registry,
) -> Result<Health, crate::Refusal> {
    let provider = registry.by_id(present.provider).for_present(present);
    let cap = provider
        .caps
        .health
        .get(index)
        .ok_or(crate::Refusal::NoCapability {
            provider: provider.id,
            op: "health",
        })?;
    let plan = crate::plan_with(
        tree,
        project,
        policy,
        present,
        &crate::Op::Health { check: index },
        registry,
    )?;
    let output = crate::execute::command(&plan)?.output()?;
    let result = (cap.parse)(&output.stdout);
    match result {
        Health::Unreadable(error) => Err(crate::Refusal::Observation {
            kind: std::io::ErrorKind::InvalidData,
            message: format!(
                "{} health in {}: {error}; status {}; {}",
                provider.label,
                plan.cwd.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }),
        Health::Ok if !output.status.success() => Err(failed(provider.label, &output)),
        Health::Problems(ref messages) if messages.is_empty() && !output.status.success() => {
            Err(failed(provider.label, &output))
        }
        result => Ok(result),
    }
}

fn failed(label: &str, output: &std::process::Output) -> crate::Refusal {
    crate::Refusal::Invalid(format!(
        "{label} health failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
