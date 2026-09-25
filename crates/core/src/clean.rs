//! Planned removal of provider-owned directories.

use std::path::PathBuf;

use crate::{Evidence, Project, Refusal, Registry, Tree};

/// Directory removals and the observations authorising them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanPlan {
    /// The directories to remove.
    pub targets: Vec<PathBuf>,
    /// Provider observations supporting these targets.
    pub because: Vec<Evidence>,
}

/// Plan directory removals from effective provider capabilities.
///
/// # Errors
/// Returns directory observation errors or a provider target outside its scope.
pub fn plan(
    tree: &Tree,
    project: &Project,
    registry: &Registry,
    frameworks: bool,
) -> Result<CleanPlan, Refusal> {
    let scope = crate::plan::scope_at(tree, &tree.cwd);
    let mut plan = CleanPlan {
        targets: Vec::new(),
        because: Vec::new(),
    };
    for present in &project.present {
        if !project
            .present_in(present.provider, &scope)
            .is_some_and(|chosen| std::ptr::eq(chosen, present))
        {
            continue;
        }
        let provider = registry.by_id(present.provider).for_present(present);
        let Some(cap) = provider.caps.clean else {
            continue;
        };
        let root = crate::plan::scope_dir(tree, &present.scope);
        let mut names: Vec<_> = cap
            .dirs
            .iter()
            .chain(cap.framework_dirs.iter().filter(|_| frameworks))
            .map(|name| PathBuf::from(*name))
            .collect();
        if !cap.dir_suffixes.is_empty() {
            for entry in std::fs::read_dir(&root).map_err(Refusal::from)? {
                let entry = entry.map_err(Refusal::from)?;
                if entry.file_name().to_str().is_some_and(|name| {
                    cap.dir_suffixes.iter().any(|suffix| name.ends_with(suffix))
                }) {
                    names.push(PathBuf::from(entry.file_name()));
                }
            }
        }
        for name in names {
            let relative = name.as_path();
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(Refusal::Invalid(format!(
                    "{} declares an invalid clean directory",
                    provider.label
                )));
            }
            let target = root.join(relative);
            match target.symlink_metadata() {
                Ok(metadata) if metadata.is_dir() && !plan.targets.contains(&target) => {
                    if present.because.is_empty() {
                        return Err(Refusal::Invalid("clean needs provider evidence".into()));
                    }
                    plan.targets.push(target);
                    plan.because.extend(present.because.iter().cloned());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("{}: {error}", target.display()),
                    )
                    .into());
                }
            }
        }
    }
    plan.targets.sort();
    Ok(plan)
}

/// Execute exactly the directory removals in a plan.
///
/// # Errors
/// Returns removal errors or missing evidence.
pub fn execute(plan: &CleanPlan) -> std::io::Result<()> {
    if !plan.targets.is_empty() && plan.because.is_empty() {
        return Err(std::io::Error::other("clean needs provider evidence"));
    }
    for target in &plan.targets {
        match std::fs::remove_dir_all(target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(std::io::Error::new(
                    error.kind(),
                    format!("{}: {error}", target.display()),
                ));
            }
        }
    }
    Ok(())
}
