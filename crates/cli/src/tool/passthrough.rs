//! Interpret provider-reported wrapper scripts.

pub(crate) fn detect_target(name: &str, command: &str) -> Option<crate::types::TaskRunner> {
    runner_providers::extract::passthrough::detect_target(name, command).and_then(|id| {
        crate::types::TaskRunner::from_label(runner_providers::REGISTRY.by_id(id).label)
    })
}
