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
            .display_order()
            .cmp(&b.source.display_order())
            .then_with(|| a.name.cmp(&b.name))
    });

    ctx
}

// Package managers

/// Detect package managers by checking for lockfiles and config files.
///
/// Node PM priority: bun > pnpm > yarn > npm > Node `packageManager` field.
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
        // Read the field with diagnostics so a present-but-unparseable
        // value (typo, unsupported PM) doesn't disappear silently —
        // emit a `DetectionWarning::UnparseablePackageManager` so the
        // user sees the raw value they wrote and can fix it.
        let (field_pm, unparseable) = tool::node::detect_pm_field_with_diagnostics(dir);
        if let Some(raw) = unparseable {
            ctx.warnings
                .push(DetectionWarning::UnparseablePackageManager { raw });
        }
        // Mirror the resolver's manifest chain: legacy `packageManager`
        // first, then `devEngines.packageManager`. When the manifest
        // declares nothing (or only an unparseable legacy field, which
        // per Corepack must not be substituted), fall back to the
        // governing lockfile/manifest of an enclosing workspace so a
        // member dir's `info`/`install` still target the right PM.
        field_pm
            .or_else(|| tool::node::detect_pm_from_manifest(dir).map(|decl| decl.pm))
            .filter(|pm| pm.is_node())
            .or_else(|| detect_node_pm_upwards(dir))
    } else {
        detect_node_pm_upwards(dir)
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

/// Walk upward — workspace-root-aware and VCS-bounded — for the package
/// manager that governs a manifest-less (or PM-less) workspace member:
/// the nearest ancestor Node lockfile, else the nearest ancestor
/// manifest's `packageManager`/`devEngines` declaration.
///
/// Returns `None` outside a JS workspace so an unrelated outer-project
/// lockfile is never adopted — the same guard that gates upward script
/// discovery, applied to PM resolution.
fn detect_node_pm_upwards(dir: &Path) -> Option<PackageManager> {
    if !tool::node::within_workspace_upwards(dir) {
        return None;
    }
    tool::files::find_in_ancestors(dir, |ancestor| {
        if tool::bun::detect(ancestor) {
            Some(PackageManager::Bun)
        } else if tool::pnpm::detect(ancestor) {
            Some(PackageManager::Pnpm)
        } else if tool::yarn::detect(ancestor) {
            Some(PackageManager::Yarn)
        } else if tool::npm::detect(ancestor) {
            Some(PackageManager::Npm)
        } else {
            tool::node::detect_pm_from_manifest(ancestor)
                .map(|decl| decl.pm)
                .filter(|pm| pm.is_node())
        }
    })
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
    if tool::bacon::detect(dir) {
        ctx.task_runners.push(TaskRunner::Bacon);
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
///
/// Each enabled extractor runs in its own scoped thread. The slow path
/// is always a subprocess wait — `just --summary --justfile <path>`,
/// `mise tasks ls`, `task --list`, `cargo metadata`, `turbo run` schema
/// reads, etc. — and those waits dominate cold-run wall-clock for any
/// project that detects more than two task sources. Serial extraction
/// costs O(N) subprocesses worth of latency; scoped parallelism brings
/// it down to ~`max(extractor_latency)` plus thread-spawn overhead.
///
/// Pushes are applied in the original declaration order so the task
/// list keeps the source ordering the resolver and snapshot tests
/// rely on. `JoinHandle::join` panics propagate the same way a panic
/// in the previous serial code would have, which is the right
/// behavior; silently swallowing a poisoned extractor would mask a
/// real bug.
fn extract_tasks(dir: &Path, ctx: &mut ProjectContext) {
    use std::thread;

    let with_deno = ctx.package_managers.contains(&PackageManager::Deno);
    // Script discovery is decoupled from package-manager detection: a
    // `package.json` *is* the Node signal; *which* PM dispatches its
    // scripts is the resolver's runtime job, not the task finder's. A
    // manifest-less subdir still lists scripts when it provably sits
    // inside a JS monorepo, so a workspace member is never met with
    // "No project detected".
    let has_local_manifest = tool::node::has_package_json(dir);
    let workspace_member = !has_local_manifest && tool::node::within_workspace_upwards(dir);
    let want_pkg_json = has_local_manifest || workspace_member || with_deno;
    let want_turbo = ctx.task_runners.contains(&TaskRunner::Turbo);
    let want_make = ctx.task_runners.contains(&TaskRunner::Make);
    let want_just = ctx.task_runners.contains(&TaskRunner::Just);
    let want_go_task = ctx.task_runners.contains(&TaskRunner::GoTask);
    let want_deno_tasks = with_deno;
    let want_cargo = ctx.package_managers.contains(&PackageManager::Cargo);
    let want_go_packages = ctx.package_managers.contains(&PackageManager::Go);
    let want_bacon = ctx.task_runners.contains(&TaskRunner::Bacon);
    let want_mise = ctx.task_runners.contains(&TaskRunner::Mise);

    thread::scope(|s| {
        let pkg_json_h = want_pkg_json.then(|| {
            s.spawn(move || {
                if has_local_manifest && !with_deno {
                    tool::node::extract_scripts(dir)
                } else {
                    tool::node::extract_scripts_upwards(dir)
                }
            })
        });
        let turbo_h = want_turbo.then(|| s.spawn(move || tool::turbo::extract_tasks(dir)));
        let make_h = want_make.then(|| s.spawn(move || tool::make::extract_tasks(dir)));
        let just_h = want_just.then(|| s.spawn(move || tool::just::extract_tasks(dir)));
        let go_task_h = want_go_task.then(|| s.spawn(move || tool::go_task::extract_tasks(dir)));
        let deno_h = want_deno_tasks.then(|| s.spawn(move || tool::deno::extract_tasks(dir)));
        let cargo_h = want_cargo.then(|| s.spawn(move || tool::cargo_aliases::extract_tasks(dir)));
        let go_h = want_go_packages.then(|| s.spawn(move || tool::go_pm::extract_tasks(dir)));
        let bacon_h = want_bacon.then(|| s.spawn(move || tool::bacon::extract_tasks(dir)));
        let mise_h = want_mise.then(|| s.spawn(move || tool::mise::extract_tasks(dir)));

        if let Some(h) = pkg_json_h {
            push_package_json_tasks(ctx, h.join().expect("extractor thread panicked"));
        }
        if let Some(h) = turbo_h {
            push_named_tasks(
                ctx,
                TaskSource::TurboJson,
                h.join().expect("extractor thread panicked"),
            );
        }
        if let Some(h) = make_h {
            push_described_tasks(
                ctx,
                TaskSource::Makefile,
                h.join().expect("extractor thread panicked"),
            );
        }
        if let Some(h) = just_h {
            push_just_tasks(ctx, h.join().expect("extractor thread panicked"));
        }
        if let Some(h) = go_task_h {
            push_described_tasks(
                ctx,
                TaskSource::Taskfile,
                h.join().expect("extractor thread panicked"),
            );
        }
        if let Some(h) = deno_h {
            push_named_tasks(
                ctx,
                TaskSource::DenoJson,
                h.join().expect("extractor thread panicked"),
            );
        }
        if let Some(h) = cargo_h {
            push_cargo_aliases(ctx, h.join().expect("extractor thread panicked"));
        }
        if let Some(h) = go_h {
            push_go_tasks(ctx, h.join().expect("extractor thread panicked"));
        }
        if let Some(h) = bacon_h {
            push_described_tasks(
                ctx,
                TaskSource::BaconToml,
                h.join().expect("extractor thread panicked"),
            );
        }
        if let Some(h) = mise_h {
            push_mise_tasks(ctx, h.join().expect("extractor thread panicked"));
        }
    });
}

fn push_go_tasks(
    ctx: &mut ProjectContext,
    result: anyhow::Result<Vec<tool::go_pm::ExtractedTask>>,
) {
    match result {
        Ok(entries) => {
            for entry in entries {
                ctx.tasks.push(Task {
                    name: entry.name,
                    source: TaskSource::GoPackage,
                    run_target: Some(entry.run_target),
                    description: None,
                    alias_of: None,
                    passthrough_to: None,
                });
            }
        }
        Err(err) => ctx.warnings.push(DetectionWarning::TaskListUnreadable {
            source: TaskSource::GoPackage.label(),
            error: format!("{err:#}"),
        }),
    }
}

/// Append tasks from the mise source, preserving alias→target metadata.
fn push_mise_tasks(
    ctx: &mut ProjectContext,
    result: anyhow::Result<Vec<tool::mise::ExtractedTask>>,
) {
    push_recipe_alias_tasks(
        ctx,
        TaskSource::MiseToml,
        result.map(|entries| entries.into_iter().map(mise_entry_triple).collect()),
    );
}

fn mise_entry_triple(entry: tool::mise::ExtractedTask) -> RecipeOrAlias {
    match entry {
        tool::mise::ExtractedTask::Recipe { name, description } => (name, description, None),
        tool::mise::ExtractedTask::Alias { name, target } => (name, None, Some(target)),
    }
}

/// Append cargo aliases as tasks. Each alias's fully recursion-expanded
/// command becomes the alias target text shown by list/why/completion.
fn push_cargo_aliases(
    ctx: &mut ProjectContext,
    result: anyhow::Result<Vec<tool::cargo_aliases::ExtractedAlias>>,
) {
    match result {
        Ok(entries) => {
            for entry in entries {
                let alias_of = Some(entry.display_command());
                ctx.tasks.push(Task {
                    name: entry.name,
                    source: TaskSource::CargoAliases,
                    run_target: None,
                    description: None,
                    alias_of,
                    passthrough_to: None,
                });
            }
        }
        Err(err) => ctx.warnings.push(DetectionWarning::TaskListUnreadable {
            source: TaskSource::CargoAliases.label(),
            error: format!("{err:#}"),
        }),
    }
}

/// Append tasks from sources that only provide names (no descriptions).
fn push_named_tasks(
    ctx: &mut ProjectContext,
    source: TaskSource,
    result: anyhow::Result<Vec<String>>,
) {
    push_described_tasks(
        ctx,
        source,
        result.map(|names| names.into_iter().map(|name| (name, None)).collect()),
    );
}

/// Append tasks from sources that provide names with optional descriptions.
fn push_described_tasks(
    ctx: &mut ProjectContext,
    source: TaskSource,
    result: anyhow::Result<Vec<(String, Option<String>)>>,
) {
    match result {
        Ok(entries) => {
            for (name, description) in entries {
                ctx.tasks.push(Task {
                    name,
                    source,
                    run_target: None,
                    description,
                    alias_of: None,
                    passthrough_to: None,
                });
            }
        }
        Err(err) => ctx.warnings.push(DetectionWarning::TaskListUnreadable {
            source: source.label(),
            error: format!("{err:#}"),
        }),
    }
}

/// Append `package.json` scripts, classifying each entry as a
/// passthrough wrapper iff its command body literally invokes a known
/// task runner against a same-named target (turbo, just, make, task,
/// nx, bacon, mise). Detection is purely textual — the surrounding
/// project state is not consulted, so a real script like
/// `"build": "vite build"` is never flagged regardless of what other
/// sources exist.
fn push_package_json_tasks(
    ctx: &mut ProjectContext,
    result: anyhow::Result<Vec<(String, String)>>,
) {
    match result {
        Ok(entries) => {
            for (name, command) in entries {
                let passthrough_to = tool::passthrough::detect_target(&name, &command);
                ctx.tasks.push(Task {
                    name,
                    source: TaskSource::PackageJson,
                    run_target: None,
                    description: None,
                    alias_of: None,
                    passthrough_to,
                });
            }
        }
        Err(err) => ctx.warnings.push(DetectionWarning::TaskListUnreadable {
            source: TaskSource::PackageJson.label(),
            error: format!("{err:#}"),
        }),
    }
}

/// Append tasks from the justfile source, preserving alias→target metadata.
fn push_just_tasks(
    ctx: &mut ProjectContext,
    result: anyhow::Result<Vec<tool::just::ExtractedTask>>,
) {
    push_recipe_alias_tasks(
        ctx,
        TaskSource::Justfile,
        result.map(|entries| entries.into_iter().map(just_entry_triple).collect()),
    );
}

fn just_entry_triple(entry: tool::just::ExtractedTask) -> RecipeOrAlias {
    match entry {
        tool::just::ExtractedTask::Recipe { name, doc } => (name, doc, None),
        tool::just::ExtractedTask::Alias { name, target } => (name, None, Some(target)),
    }
}

/// Flattened `(name, description, alias_of)` shape both
/// `tool::mise::ExtractedTask` and `tool::just::ExtractedTask` collapse
/// to before they hit [`push_recipe_alias_tasks`].
type RecipeOrAlias = (String, Option<String>, Option<String>);

/// Push `(name, description, alias_of)` triples into `ctx.tasks` under
/// `source`, or record a `TaskListUnreadable` warning on error. Shared
/// by [`push_mise_tasks`] and [`push_just_tasks`] — both runners emit
/// recipe-or-alias variants that flatten to the same triple shape.
fn push_recipe_alias_tasks(
    ctx: &mut ProjectContext,
    source: TaskSource,
    result: anyhow::Result<Vec<RecipeOrAlias>>,
) {
    match result {
        Ok(entries) => {
            for (name, description, alias_of) in entries {
                ctx.tasks.push(Task {
                    name,
                    source,
                    run_target: None,
                    description,
                    alias_of,
                    passthrough_to: None,
                });
            }
        }
        Err(err) => ctx.warnings.push(DetectionWarning::TaskListUnreadable {
            source: source.label(),
            error: format!("{err:#}"),
        }),
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
        assert_eq!(ctx.warnings[0].source(), "turbo");
    }

    #[test]
    fn detect_records_warning_for_unparseable_package_manager_field() {
        // The user typo'd `pnpm` → `pnpmm`. The resolver can't dispatch
        // through `pnpmm@9`, so the manifest declaration is ignored —
        // but the detection layer surfaces the raw value verbatim so
        // the user sees their typo instead of staring at a doctor
        // report that just shows `manifest_pm: null`.
        let dir = TempDir::new("detect-unparseable-pm-field");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "pnpmm@9.0.0" }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());

        let detail = ctx
            .warnings
            .iter()
            .find_map(|w| {
                matches!(
                    w,
                    crate::types::DetectionWarning::UnparseablePackageManager { .. }
                )
                .then(|| w.detail())
            })
            .expect("unparseable-packageManager warning should be emitted");
        assert!(
            detail.contains("pnpmm@9.0.0"),
            "warning should echo the raw value verbatim: {detail}",
        );
        assert!(
            detail.contains("npm|pnpm|yarn|bun|deno"),
            "warning should list the accepted values: {detail}",
        );
    }

    #[test]
    fn detect_models_cargo_aliases_as_aliases() {
        let dir = TempDir::new("detect-cargo-alias-shape");
        let cargo_dir = dir.path().join(".cargo");
        fs::create_dir_all(&cargo_dir).expect(".cargo dir should be created");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("Cargo.toml should be written");
        fs::write(
            cargo_dir.join("config.toml"),
            "[alias]\nl = \"clippy --all-targets\"\n",
        )
        .expect("config.toml should be written");

        let ctx = detect(dir.path());
        let task = ctx
            .tasks
            .iter()
            .find(|task| task.source == crate::types::TaskSource::CargoAliases && task.name == "l")
            .expect("cargo alias should be detected");

        assert_eq!(task.description, None);
        assert_eq!(task.alias_of.as_deref(), Some("clippy --all-targets"));
    }

    #[test]
    fn detect_models_go_cmd_packages_as_tasks() {
        let dir = TempDir::new("detect-go-cmd-package");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        let cmd_dir = dir.path().join("cmd").join("serve");
        fs::create_dir_all(&cmd_dir).expect("cmd package dir should be created");
        fs::write(cmd_dir.join("main.go"), "package main\n\nfunc main() {}\n")
            .expect("main.go should be written");

        let ctx = detect(dir.path());

        assert!(ctx.tasks.iter().any(|task| {
            task.source == crate::types::TaskSource::GoPackage
                && task.name == "serve"
                && task.run_target.as_deref() == Some("./cmd/serve")
        }));
    }

    #[test]
    fn detect_models_root_go_main_package_as_task() {
        let dir = TempDir::new("detect-go-root-package");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("main.go should be written");

        let ctx = detect(dir.path());
        let name = dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temp dir should have utf-8 file name");

        assert!(ctx.tasks.iter().any(|task| {
            task.source == crate::types::TaskSource::GoPackage
                && task.name == name
                && task.run_target.as_deref() == Some(".")
        }));
    }

    #[test]
    fn detect_uses_deno_for_package_json_deno_projects() {
        let dir = TempDir::new("detect-package-json-deno");
        fs::write(
            dir.path().join("package.json"),
            r#"{
  "packageManager": "deno@2.7.12",
  "scripts": {
    "build": "vite build"
  }
}"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());

        assert_eq!(ctx.package_managers, [crate::types::PackageManager::Deno]);
        assert!(ctx.tasks.iter().any(
            |task| task.source == crate::types::TaskSource::PackageJson && task.name == "build"
        ));
    }

    #[test]
    fn detect_uses_nearest_deno_sources_from_nested_dir() {
        let dir = TempDir::new("detect-deno-nearest");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(dir.path().join("deno.lock"), "{}").expect("deno.lock should be written");
        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ tasks: { root: "deno task root" } }"#,
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{
  "scripts": {
    "member": "deno task member"
  }
}"#,
        )
        .expect("member package.json should be written");

        let ctx = detect(&nested);

        assert!(
            ctx.package_managers
                .contains(&crate::types::PackageManager::Deno)
        );
        assert!(ctx.tasks.iter().any(|task| task.name == "member"));
        assert!(ctx.tasks.iter().any(|task| task.name == "root"));
    }

    #[test]
    fn detect_lists_scripts_without_lockfile_or_pm_field() {
        // Headline regression: a `package.json` with scripts but no
        // lockfile and no `packageManager`/`devEngines` field (a typical
        // pnpm-workspace member dir) used to detect zero node PMs and so
        // skip script extraction entirely → "No project detected".
        let dir = TempDir::new("detect-scripts-no-pm-signal");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "name": "leaf", "scripts": { "build": "wxt build" } }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());

        assert!(
            ctx.package_managers.is_empty(),
            "no lockfile/pm field → no PM detected, yet scripts must still list",
        );
        assert!(ctx.tasks.iter().any(
            |task| task.source == crate::types::TaskSource::PackageJson && task.name == "build"
        ));
    }

    #[test]
    fn detect_lists_workspace_member_scripts_from_manifestless_subdir() {
        // Workspace-root-aware upward walk: a manifest-less subdir inside
        // a monorepo (root `pnpm-workspace.yaml`) adopts the nearest
        // ancestor manifest's scripts.
        let dir = TempDir::new("detect-workspace-member-subdir");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .expect("pnpm-workspace.yaml should be written");
        let member = dir.path().join("apps").join("ext");
        let nested = member.join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            member.join("package.json"),
            r#"{ "scripts": { "ext-build": "wxt build" } }"#,
        )
        .expect("member package.json should be written");

        let ctx = detect(&nested);

        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.source == crate::types::TaskSource::PackageJson
                    && task.name == "ext-build")
        );
    }

    #[test]
    fn detect_skips_ancestor_manifest_outside_a_workspace() {
        // The workspace-root-aware guard: a manifest-less subdir with NO
        // workspace marker must NOT silently adopt an unrelated ancestor
        // `package.json` from some outer project.
        let dir = TempDir::new("detect-no-workspace-no-adopt");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "root-only": "echo nope" } }"#,
        )
        .expect("ancestor package.json should be written");
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).expect("subdir should be created");

        let ctx = detect(&sub);

        assert!(
            !ctx.tasks.iter().any(|task| task.name == "root-only"),
            "no workspace marker → ancestor manifest must not be adopted",
        );
    }

    #[test]
    fn detect_pm_from_dev_engines_without_lockfile() {
        // devEngines-only manifest (no lockfile, no legacy
        // packageManager) must resolve a node PM so info/install work.
        let dir = TempDir::new("detect-dev-engines-pm");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "pnpm", "version": "9" } },
                 "scripts": { "build": "vite build" } }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path());

        assert_eq!(ctx.package_managers, [crate::types::PackageManager::Pnpm]);
        assert!(ctx.tasks.iter().any(
            |task| task.source == crate::types::TaskSource::PackageJson && task.name == "build"
        ));
    }

    #[test]
    fn detect_pm_upwards_for_workspace_member() {
        // A member dir with its own lockfile-less, PM-less package.json
        // inside a pnpm workspace whose root carries the lockfile: the
        // member must inherit the root's pnpm so `runner install` here
        // doesn't fall back to the wrong manager.
        let dir = TempDir::new("detect-pm-upwards-member");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .expect("pnpm-workspace.yaml should be written");
        fs::write(
            dir.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .expect("root pnpm-lock.yaml should be written");
        let member = dir.path().join("apps").join("ext");
        fs::create_dir_all(&member).expect("member dir should be created");
        fs::write(
            member.join("package.json"),
            r#"{ "name": "ext", "scripts": { "build": "wxt build" } }"#,
        )
        .expect("member package.json should be written");

        let ctx = detect(&member);

        assert_eq!(ctx.package_managers, [crate::types::PackageManager::Pnpm]);
        assert!(ctx.tasks.iter().any(
            |task| task.source == crate::types::TaskSource::PackageJson && task.name == "build"
        ));
    }
}
