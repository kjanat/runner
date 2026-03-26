//! Project detection: scans the working directory for config/lock files and
//! builds a [`ProjectContext`] describing the detected toolchain.

use std::path::Path;
use std::process;

use serde::Deserialize;

use crate::tool;
use crate::types::{
    DetectionWarning, NodeVersion, PackageManager, ProjectContext, Task, TaskRunner, TaskSource,
};

/// Scan `dir` for known config/lock files and return a populated [`ProjectContext`].
///
/// Detection order:
/// 1. Package managers (Node lockfiles take priority over `package.json` field)
/// 2. Task runners
/// 3. Node.js version constraints
/// 4. Monorepo indicators
/// 5. Task extraction (conditional on detected tools)
pub(crate) fn detect(dir: &Path) -> ProjectContext {
    let mut ctx = ProjectContext {
        root: dir.to_path_buf(),
        package_managers: Vec::new(),
        task_runners: Vec::new(),
        tasks: Vec::new(),
        node_version: None,
        current_node: None,
        is_monorepo: false,
        warnings: Vec::new(),
    };

    detect_package_managers(dir, &mut ctx);
    detect_task_runners(dir, &mut ctx);
    detect_node_version(dir, &mut ctx);
    detect_monorepo(dir, &mut ctx);
    extract_tasks(dir, &mut ctx);

    ctx.tasks.sort_by(|a, b| {
        a.source
            .priority()
            .cmp(&b.source.priority())
            .then_with(|| a.name.cmp(&b.name))
    });

    ctx
}

// Package managers

/// Detect package managers by checking for lockfiles and config files.
///
/// Node PM priority: bun > pnpm > yarn > npm > `packageManager` field.
/// Within non-Node ecosystems, multiple PMs can coexist (e.g. Cargo + npm).
fn detect_package_managers(dir: &Path, ctx: &mut ProjectContext) {
    let node_pm = if tool::bun::detect(dir) {
        Some(PackageManager::Bun)
    } else if tool::pnpm::detect(dir) {
        Some(PackageManager::Pnpm)
    } else if tool::yarn::detect(dir) {
        Some(PackageManager::Yarn)
    } else if tool::npm::detect(dir) {
        Some(PackageManager::Npm)
    } else if tool::node::has_package_json(dir) {
        Some(tool::node::detect_pm_from_field(dir))
    } else {
        None
    };
    if let Some(pm) = node_pm {
        ctx.package_managers.push(pm);
    }

    if tool::cargo_pm::detect(dir) {
        ctx.package_managers.push(PackageManager::Cargo);
    }
    if tool::deno::detect(dir) {
        ctx.package_managers.push(PackageManager::Deno);
    }
    if tool::uv::detect(dir) {
        ctx.package_managers.push(PackageManager::Uv);
    } else if tool::poetry::detect(dir) {
        ctx.package_managers.push(PackageManager::Poetry);
    } else if tool::pipenv::detect(dir) {
        ctx.package_managers.push(PackageManager::Pipenv);
    }
    if tool::go_pm::detect(dir) {
        ctx.package_managers.push(PackageManager::Go);
    }
    if tool::bundler::detect(dir) {
        ctx.package_managers.push(PackageManager::Bundler);
    }
    if tool::composer::detect(dir) {
        ctx.package_managers.push(PackageManager::Composer);
    }
}

// Task runners

/// Detect task runners by checking for their config files.
fn detect_task_runners(dir: &Path, ctx: &mut ProjectContext) {
    if tool::turbo::detect(dir) {
        ctx.task_runners.push(TaskRunner::Turbo);
    }
    if tool::nx::detect(dir) {
        ctx.task_runners.push(TaskRunner::Nx);
    }
    if tool::make::detect(dir) {
        ctx.task_runners.push(TaskRunner::Make);
    }
    if tool::just::detect(dir) {
        ctx.task_runners.push(TaskRunner::Just);
    }
    if tool::go_task::detect(dir) {
        ctx.task_runners.push(TaskRunner::GoTask);
    }
    if tool::mise::detect(dir) {
        ctx.task_runners.push(TaskRunner::Mise);
    }
}

// Node version

/// Detect the expected Node.js version from version files and the current
/// installed version via `node --version`.
///
/// Sources checked (first match wins):
/// 1. `.nvmrc`
/// 2. `.node-version`
/// 3. `.tool-versions` (asdf `nodejs` key)
/// 4. `package.json` `"engines.node"`
fn detect_node_version(dir: &Path, ctx: &mut ProjectContext) {
    for (file, source) in [(".nvmrc", ".nvmrc"), (".node-version", ".node-version")] {
        if let Ok(raw) = std::fs::read_to_string(dir.join(file)) {
            let v = raw.trim();
            if !v.is_empty() {
                ctx.node_version = Some(NodeVersion {
                    expected: v.strip_prefix('v').unwrap_or(v).to_string(),
                    source,
                });
                break;
            }
        }
    }

    if ctx.node_version.is_none()
        && let Ok(content) = std::fs::read_to_string(dir.join(".tool-versions"))
    {
        for line in content.lines() {
            if let Some(v) = parse_tool_versions_node(line) {
                ctx.node_version = Some(NodeVersion {
                    expected: v.to_string(),
                    source: ".tool-versions",
                });
                break;
            }
        }
    }

    if ctx.node_version.is_none()
        && let Ok(content) = std::fs::read_to_string(dir.join("package.json"))
    {
        #[derive(Deserialize)]
        struct Engines {
            node: Option<String>,
        }
        #[derive(Deserialize)]
        struct Partial {
            engines: Option<Engines>,
        }
        if let Ok(p) = serde_json::from_str::<Partial>(&content)
            && let Some(v) = p.engines.and_then(|e| e.node)
        {
            ctx.node_version = Some(NodeVersion {
                expected: v,
                source: "package.json engines",
            });
        }
    }

    if ctx.node_version.is_some() || ctx.package_managers.iter().any(|pm| pm.is_node()) {
        ctx.current_node = detect_current_node();
    }
}

/// Shell out to `node --version` and parse the result.
fn detect_current_node() -> Option<String> {
    let out = process::Command::new("node")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let trimmed = raw.trim();
    let v = trimmed.strip_prefix('v').unwrap_or(trimmed);
    Some(v.to_string())
}

fn parse_tool_versions_node(line: &str) -> Option<&str> {
    let content = line.split('#').next()?.trim();
    let mut parts = content.split_whitespace();
    let tool = parts.next()?;
    let version = parts.next()?;
    (tool == "nodejs").then_some(version)
}

// Monorepo

/// Check for monorepo indicators: workspace configs, turbo, nx, cargo workspace.
fn detect_monorepo(dir: &Path, ctx: &mut ProjectContext) {
    if dir.join("pnpm-workspace.yaml").exists() || dir.join("lerna.json").exists() {
        ctx.is_monorepo = true;
    }
    if ctx.task_runners.contains(&TaskRunner::Turbo) || ctx.task_runners.contains(&TaskRunner::Nx) {
        ctx.is_monorepo = true;
    }
    if tool::cargo_pm::detect_workspace(dir) {
        ctx.is_monorepo = true;
    }
    if let Ok(content) = std::fs::read_to_string(dir.join("package.json"))
        && let Ok(p) = serde_json::from_str::<serde_json::Value>(&content)
        && p.get("workspaces").is_some()
    {
        ctx.is_monorepo = true;
    }
}

// Task extraction

/// Extract tasks only from tools that were actually detected, avoiding
/// unnecessary filesystem reads.
fn extract_tasks(dir: &Path, ctx: &mut ProjectContext) {
    if ctx.package_managers.iter().any(|pm| pm.is_node()) {
        push_extracted_tasks(
            ctx,
            TaskSource::PackageJson,
            tool::node::extract_scripts(dir),
        );
    }
    if ctx.task_runners.contains(&TaskRunner::Turbo) {
        push_extracted_tasks(ctx, TaskSource::TurboJson, tool::turbo::extract_tasks(dir));
    }
    if ctx.task_runners.contains(&TaskRunner::Make) {
        push_extracted_tasks(ctx, TaskSource::Makefile, tool::make::extract_tasks(dir));
    }
    if ctx.task_runners.contains(&TaskRunner::Just) {
        push_extracted_tasks(ctx, TaskSource::Justfile, tool::just::extract_tasks(dir));
    }
    if ctx.task_runners.contains(&TaskRunner::GoTask) {
        push_extracted_tasks(ctx, TaskSource::Taskfile, tool::go_task::extract_tasks(dir));
    }
    if ctx.package_managers.contains(&PackageManager::Deno) {
        push_extracted_tasks(ctx, TaskSource::DenoJson, tool::deno::extract_tasks(dir));
    }
}

fn push_extracted_tasks(
    ctx: &mut ProjectContext,
    source: TaskSource,
    result: anyhow::Result<Vec<String>>,
) {
    match result {
        Ok(names) => push_tasks(&mut ctx.tasks, source, names),
        Err(err) => ctx.warnings.push(DetectionWarning {
            source: source.label(),
            detail: format!("failed to read tasks: {err:#}"),
        }),
    }
}

/// Convert a vec of task names into [`Task`] structs and append them.
fn push_tasks(tasks: &mut Vec<Task>, source: TaskSource, names: Vec<String>) {
    for name in names {
        tasks.push(Task { name, source });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::parse_tool_versions_node;
    use crate::detect::detect;
    use crate::tool::test_support::TempDir;

    #[test]
    fn parses_tool_versions_node_entry() {
        assert_eq!(parse_tool_versions_node("nodejs 20.11.1"), Some("20.11.1"));
    }

    #[test]
    fn ignores_malformed_tool_versions_entry() {
        assert_eq!(parse_tool_versions_node("nodejs20.11.1"), None);
    }

    #[test]
    fn strips_tool_versions_inline_comments() {
        assert_eq!(
            parse_tool_versions_node("nodejs 20.11.1 # pinned for ci"),
            Some("20.11.1")
        );
    }

    #[test]
    fn detect_records_warnings_for_invalid_task_configs() {
        let dir = TempDir::new("detect-warning");
        fs::write(dir.path().join("turbo.json"), "{").expect("turbo.json should be written");

        let ctx = detect(dir.path());

        assert_eq!(ctx.warnings.len(), 1);
        assert_eq!(ctx.warnings[0].source, "turbo.json");
    }
}
