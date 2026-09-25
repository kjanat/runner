//! Provider-owned task discovery.

pub mod bacon;
pub mod files;
pub mod go_task;
pub mod just;
pub mod make;
pub mod mise;
#[cfg(test)]
mod test_support;
pub mod turbo;

fn command(program: &str) -> std::process::Command {
    std::process::Command::new(
        runner_core::probe_with(program, &[]).unwrap_or_else(|| program.into()),
    )
}

fn task(
    present: &runner_core::Present,
    name: String,
    description: Option<String>,
) -> runner_core::Task {
    runner_core::Task {
        name,
        description,
        source: present.provider,
        scope: present.scope.clone(),
        target: None,
        alias_of: None,
        forwards_to: None,
        detail: runner_core::TaskDetail {
            source: present
                .because
                .iter()
                .find(|e| e.weight != runner_core::Weight::Probed)
                .map(|e| e.at.clone()),
            ..runner_core::TaskDetail::default()
        },
    }
}

pub mod cargo_aliases;

pub mod go_pm;

pub mod passthrough;
pub mod scripts;
