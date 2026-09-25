//! `runner clean`, remove caches and build artifacts for detected tools.

use std::io;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::render::out::Out;
use crate::resolver::ResolutionOverrides;
use crate::types::ProjectContext;

/// Collect ecosystem-specific directories that exist under the project root,
/// prompt for confirmation (unless `skip_confirm`), then delete them.
///
/// A captured `out` declines the prompt.
pub(crate) fn clean(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    skip_confirm: bool,
    include_framework: bool,
    out: &mut Out<'_>,
) -> Result<()> {
    let tree = super::run::core::tree(ctx);
    let project = super::run::core::project_under(ctx, &super::run::core::policy(overrides))?;
    let plan = runner_core::clean::plan(
        &tree,
        &project,
        &runner_providers::REGISTRY,
        include_framework,
    )?;
    let targets: Vec<String> = plan
        .targets
        .iter()
        .map(|path| {
            path.strip_prefix(&ctx.root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    if overrides.explain {
        super::print_explain(
            overrides,
            &format!("clean at {}: {:?}", ctx.root.display(), targets),
        );
        return Ok(());
    }

    if targets.is_empty() {
        if overrides.shows_progress() {
            writeln!(out.stdout(), "{}", "Nothing to clean.".dimmed())?;
        }
        return Ok(());
    }

    if !skip_confirm && !overrides.shows_progress() {
        bail!("clean requires --yes when runner progress is hidden");
    }

    if overrides.shows_progress() {
        writeln!(out.stdout(), "Will remove:")?;
        for t in &targets {
            writeln!(out.stdout(), "  {t}")?;
        }
    }

    if !skip_confirm {
        write!(out.stdout(), "\nProceed? [y/N] ")?;
        out.stdout().flush()?;
        let mut input = String::new();
        if matches!(out, Out::Stdio(..)) {
            io::stdin().read_line(&mut input)?;
        }
        if !input.trim().eq_ignore_ascii_case("y") {
            writeln!(out.stdout(), "Aborted.")?;
            return Ok(());
        }
    }

    runner_core::clean::execute(&plan)?;
    if overrides.shows_progress() {
        for target in &targets {
            writeln!(out.stdout(), "  {} {target}", "removed".red())?;
        }
    }

    Ok(())
}

#[cfg(test)]
fn collect_targets(ctx: &ProjectContext, include_framework: bool) -> Vec<String> {
    crate::tool::test_support::seed_context(ctx);
    let tree = super::run::core::tree(ctx);
    let project = super::run::core::project_under(ctx, &runner_core::Policy::default()).unwrap();
    runner_core::clean::plan(
        &tree,
        &project,
        &runner_providers::REGISTRY,
        include_framework,
    )
    .unwrap()
    .targets
    .iter()
    .map(|path| path.strip_prefix(&ctx.root).unwrap().display().to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::collect_targets;
    use crate::tool::test_support::TempDir;
    use crate::types::ProjectContext;
    use crate::types::{PackageManager, TaskRunner};

    fn context(root: &std::path::Path) -> ProjectContext {
        ProjectContext {
            cwd: root.to_path_buf(),
            root: root.to_path_buf(),
            package_managers: vec![PackageManager::Npm],
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn collect_targets_skips_framework_dirs_by_default() {
        let dir = TempDir::new("clean-node-default");
        fs::create_dir(dir.path().join("node_modules")).expect("node_modules should be created");
        fs::create_dir(dir.path().join(".next")).expect(".next should be created");

        let targets = collect_targets(&context(dir.path()), false);

        assert_eq!(targets, ["node_modules"]);
    }

    #[test]
    fn collect_targets_includes_framework_dirs_on_opt_in() {
        let dir = TempDir::new("clean-node-framework");
        fs::create_dir(dir.path().join("node_modules")).expect("node_modules should be created");
        fs::create_dir(dir.path().join(".next")).expect(".next should be created");

        let targets = collect_targets(&context(dir.path()), true);

        assert_eq!(targets, [".next", "node_modules"]);
    }

    #[test]
    fn collect_targets_still_includes_task_runner_dirs() {
        let dir = TempDir::new("clean-task-runner");
        fs::create_dir(dir.path().join(".turbo")).expect(".turbo should be created");

        let mut ctx = context(dir.path());
        ctx.package_managers.clear();
        ctx.task_runners = vec![TaskRunner::Turbo];

        let targets = collect_targets(&ctx, false);

        assert_eq!(targets, [".turbo"]);
    }

    #[test]
    fn collect_targets_skips_files_named_like_artifact_dirs() {
        let dir = TempDir::new("clean-file-target");
        fs::write(dir.path().join("node_modules"), "nope")
            .expect("node_modules file should be written");

        let targets = collect_targets(&context(dir.path()), false);

        assert_eq!(targets.len(), 0);
    }

    #[test]
    fn collect_targets_uses_python_file_evidence_without_a_package_manager() {
        let dir = TempDir::new("clean-python-generic");
        fs::write(dir.path().join("requirements.txt"), "pytest\n")
            .expect("requirements.txt should be written");
        fs::create_dir(dir.path().join("dist")).expect("dist should be created");
        fs::create_dir(dir.path().join("pkg.egg-info")).expect("pkg.egg-info should be created");

        let mut ctx = context(dir.path());
        ctx.package_managers.clear();

        let targets = collect_targets(&ctx, false);

        assert_eq!(targets, ["dist", "pkg.egg-info"]);
    }

    #[test]
    fn collect_targets_cleans_nothing_without_provider_evidence() {
        let dir = TempDir::new("clean-unobserved");
        fs::create_dir(dir.path().join("dist")).unwrap();
        let mut ctx = context(dir.path());
        ctx.package_managers.clear();
        assert_eq!(collect_targets(&ctx, false).len(), 0);
    }
}
