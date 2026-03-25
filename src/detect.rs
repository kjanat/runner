use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, process};

use anyhow::Result;
use serde::Deserialize;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Bun,
    Cargo,
    Deno,
    Uv,
    Poetry,
    Pipenv,
    Go,
    Bundler,
    Composer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskRunner {
    Turbo,
    Nx,
    Make,
    Just,
    GoTask,
    Mise,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub name: String,
    pub source: TaskSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskSource {
    PackageJson,
    Makefile,
    Justfile,
    Taskfile,
    TurboJson,
    DenoJson,
}

#[derive(Debug, Clone)]
pub struct NodeVersion {
    pub expected: String,
    pub source: &'static str,
}

pub struct ProjectContext {
    pub root: PathBuf,
    pub package_managers: Vec<PackageManager>,
    pub task_runners: Vec<TaskRunner>,
    pub tasks: Vec<Task>,
    pub node_version: Option<NodeVersion>,
    pub current_node: Option<String>,
    pub is_monorepo: bool,
}

impl ProjectContext {
    /// First Node-ecosystem PM, or first PM of any kind.
    pub fn primary_node_pm(&self) -> Option<PackageManager> {
        self.package_managers
            .iter()
            .copied()
            .find(|pm| pm.is_node())
    }

    pub fn primary_pm(&self) -> Option<PackageManager> {
        self.package_managers.first().copied()
    }
}

impl PackageManager {
    pub fn is_node(self) -> bool {
        matches!(self, Self::Npm | Self::Yarn | Self::Pnpm | Self::Bun)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
            Self::Bun => "bun",
            Self::Cargo => "cargo",
            Self::Deno => "deno",
            Self::Uv => "uv",
            Self::Poetry => "poetry",
            Self::Pipenv => "pipenv",
            Self::Go => "go",
            Self::Bundler => "bundler",
            Self::Composer => "composer",
        }
    }
}

impl TaskRunner {
    pub fn label(self) -> &'static str {
        match self {
            Self::Turbo => "turbo",
            Self::Nx => "nx",
            Self::Make => "make",
            Self::Just => "just",
            Self::GoTask => "task",
            Self::Mise => "mise",
        }
    }
}

impl TaskSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::PackageJson => "package.json",
            Self::Makefile => "Makefile",
            Self::Justfile => "justfile",
            Self::Taskfile => "Taskfile",
            Self::TurboJson => "turbo.json",
            Self::DenoJson => "deno.json",
        }
    }
}

// ── Detection entry point ──────────────────────────────────────────────

pub fn detect(dir: &Path) -> Result<ProjectContext> {
    let mut ctx = ProjectContext {
        root: dir.to_path_buf(),
        package_managers: Vec::new(),
        task_runners: Vec::new(),
        tasks: Vec::new(),
        node_version: None,
        current_node: None,
        is_monorepo: false,
    };

    detect_package_managers(dir, &mut ctx);
    detect_task_runners(dir, &mut ctx);
    detect_node_version(dir, &mut ctx);
    detect_monorepo(dir, &mut ctx);
    extract_tasks(dir, &mut ctx);

    // Sort tasks: by source ordinal, then name
    ctx.tasks.sort_by(|a, b| {
        (a.source as u8)
            .cmp(&(b.source as u8))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(ctx)
}

// ── Package manager detection ──────────────────────────────────────────

fn detect_package_managers(dir: &Path, ctx: &mut ProjectContext) {
    // Node — lockfile takes priority
    let node_pm = if exists(dir, "bun.lockb") || exists(dir, "bun.lock") {
        Some(PackageManager::Bun)
    } else if exists(dir, "pnpm-lock.yaml") {
        Some(PackageManager::Pnpm)
    } else if exists(dir, "yarn.lock") {
        Some(PackageManager::Yarn)
    } else if exists(dir, "package-lock.json") {
        Some(PackageManager::Npm)
    } else if exists(dir, "package.json") {
        Some(detect_node_pm_from_package_json(dir))
    } else {
        None
    };
    if let Some(pm) = node_pm {
        ctx.package_managers.push(pm);
    }

    // Rust
    if exists(dir, "Cargo.toml") {
        ctx.package_managers.push(PackageManager::Cargo);
    }

    // Deno
    if exists(dir, "deno.json") || exists(dir, "deno.jsonc") {
        ctx.package_managers.push(PackageManager::Deno);
    }

    // Python
    if exists(dir, "uv.lock") {
        ctx.package_managers.push(PackageManager::Uv);
    } else if exists(dir, "poetry.lock") {
        ctx.package_managers.push(PackageManager::Poetry);
    } else if exists(dir, "Pipfile") || exists(dir, "Pipfile.lock") {
        ctx.package_managers.push(PackageManager::Pipenv);
    }

    // Go
    if exists(dir, "go.mod") {
        ctx.package_managers.push(PackageManager::Go);
    }

    // Ruby
    if exists(dir, "Gemfile") {
        ctx.package_managers.push(PackageManager::Bundler);
    }

    // PHP
    if exists(dir, "composer.json") {
        ctx.package_managers.push(PackageManager::Composer);
    }
}

fn detect_node_pm_from_package_json(dir: &Path) -> PackageManager {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(rename = "packageManager")]
        package_manager: Option<String>,
    }
    let Ok(bytes) = fs::read_to_string(dir.join("package.json")) else {
        return PackageManager::Npm;
    };
    let Ok(p) = serde_json::from_str::<Partial>(&bytes) else {
        return PackageManager::Npm;
    };
    match p.package_manager.as_deref() {
        Some(s) if s.starts_with("pnpm") => PackageManager::Pnpm,
        Some(s) if s.starts_with("yarn") => PackageManager::Yarn,
        Some(s) if s.starts_with("bun") => PackageManager::Bun,
        _ => PackageManager::Npm,
    }
}

// ── Task runner detection ──────────────────────────────────────────────

fn detect_task_runners(dir: &Path, ctx: &mut ProjectContext) {
    if exists(dir, "turbo.json") {
        ctx.task_runners.push(TaskRunner::Turbo);
    }
    if exists(dir, "nx.json") {
        ctx.task_runners.push(TaskRunner::Nx);
    }
    if any_exists(dir, &["Makefile", "GNUmakefile", "makefile"]) {
        ctx.task_runners.push(TaskRunner::Make);
    }
    if any_exists(dir, &["justfile", "Justfile", ".justfile"]) {
        ctx.task_runners.push(TaskRunner::Just);
    }
    if any_exists(dir, &["Taskfile.yml", "Taskfile.yaml", "taskfile.yml"]) {
        ctx.task_runners.push(TaskRunner::GoTask);
    }
    if exists(dir, "mise.toml") || exists(dir, ".mise.toml") {
        ctx.task_runners.push(TaskRunner::Mise);
    }
}

// ── Node version detection ─────────────────────────────────────────────

fn detect_node_version(dir: &Path, ctx: &mut ProjectContext) {
    // .nvmrc / .node-version
    for (file, source) in [(".nvmrc", ".nvmrc"), (".node-version", ".node-version")] {
        if let Some(raw) = read_trimmed(dir, file) {
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

    // .tool-versions (asdf)
    if ctx.node_version.is_none()
        && let Some(content) = read_trimmed(dir, ".tool-versions")
    {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("nodejs") {
                let v = rest.trim();
                if !v.is_empty() {
                    ctx.node_version = Some(NodeVersion {
                        expected: v.to_string(),
                        source: ".tool-versions",
                    });
                    break;
                }
            }
        }
    }

    // package.json engines.node
    if ctx.node_version.is_none()
        && let Some(content) = read_trimmed(dir, "package.json")
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

    // Current node version (only bother if we found an expected version)
    if ctx.node_version.is_some() || ctx.package_managers.iter().any(|pm| pm.is_node()) {
        ctx.current_node = detect_current_node();
    }
}

fn detect_current_node() -> Option<String> {
    let out = process::Command::new("node")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let v = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    Some(v.to_string())
}

// ── Monorepo detection ─────────────────────────────────────────────────

fn detect_monorepo(dir: &Path, ctx: &mut ProjectContext) {
    if exists(dir, "pnpm-workspace.yaml") || exists(dir, "lerna.json") {
        ctx.is_monorepo = true;
    }
    if ctx.task_runners.contains(&TaskRunner::Turbo) || ctx.task_runners.contains(&TaskRunner::Nx) {
        ctx.is_monorepo = true;
    }
    // Cargo workspace
    if let Some(content) = read_trimmed(dir, "Cargo.toml")
        && content.contains("[workspace]")
    {
        ctx.is_monorepo = true;
    }
    // package.json workspaces
    if let Some(content) = read_trimmed(dir, "package.json")
        && let Ok(p) = serde_json::from_str::<serde_json::Value>(&content)
        && p.get("workspaces").is_some()
    {
        ctx.is_monorepo = true;
    }
}

// ── Task extraction ────────────────────────────────────────────────────

fn extract_tasks(dir: &Path, ctx: &mut ProjectContext) {
    extract_package_json_scripts(dir, ctx);
    extract_turbo_tasks(dir, ctx);
    extract_makefile_targets(dir, ctx);
    extract_justfile_recipes(dir, ctx);
    extract_taskfile_tasks(dir, ctx);
    extract_deno_tasks(dir, ctx);
}

fn extract_package_json_scripts(dir: &Path, ctx: &mut ProjectContext) {
    let Some(content) = read_trimmed(dir, "package.json") else {
        return;
    };
    #[derive(Deserialize)]
    struct Partial {
        scripts: Option<HashMap<String, String>>,
    }
    let Ok(p) = serde_json::from_str::<Partial>(&content) else {
        return;
    };
    if let Some(scripts) = p.scripts {
        for name in scripts.keys() {
            ctx.tasks.push(Task {
                name: name.clone(),
                source: TaskSource::PackageJson,
            });
        }
    }
}

fn extract_turbo_tasks(dir: &Path, ctx: &mut ProjectContext) {
    let Some(content) = read_trimmed(dir, "turbo.json") else {
        return;
    };
    // turbo v2 uses "tasks", v1 used "pipeline"
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
        pipeline: Option<HashMap<String, serde_json::Value>>,
    }
    let Ok(p) = serde_json::from_str::<Partial>(&content) else {
        return;
    };
    let Some(tasks) = p.tasks.or(p.pipeline) else {
        return;
    };
    for name in tasks.keys() {
        // Skip scoped tasks (workspace#task)
        if !name.contains('#') {
            ctx.tasks.push(Task {
                name: name.clone(),
                source: TaskSource::TurboJson,
            });
        }
    }
}

fn extract_makefile_targets(dir: &Path, ctx: &mut ProjectContext) {
    let path = first_existing(dir, &["Makefile", "GNUmakefile", "makefile"]);
    let Some(content) = path.and_then(|p| fs::read_to_string(p).ok()) else {
        return;
    };
    for line in content.lines() {
        if line.starts_with('\t') || line.starts_with(' ') || line.starts_with('#') {
            continue;
        }
        // Skip special targets
        if line.starts_with('.') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        // Skip := and ::=
        let after = &line[colon..];
        if after.starts_with(":=") || after.starts_with("::") {
            continue;
        }
        let target = line[..colon].trim();
        if !target.is_empty()
            && !target.contains(' ')
            && !target.contains('$')
            && !target.contains('%')
        {
            ctx.tasks.push(Task {
                name: target.to_string(),
                source: TaskSource::Makefile,
            });
        }
    }
}

fn extract_justfile_recipes(dir: &Path, ctx: &mut ProjectContext) {
    let path = first_existing(dir, &["justfile", "Justfile", ".justfile"]);
    let Some(content) = path.and_then(|p| fs::read_to_string(p).ok()) else {
        return;
    };
    for line in content.lines() {
        // Skip indented lines (recipe bodies), comments, directives
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("set ")
            || trimmed.starts_with("alias ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("mod ")
            || trimmed.starts_with("export ")
        {
            continue;
        }
        // Strip leading @ (quiet recipe)
        let recipe = trimmed.strip_prefix('@').unwrap_or(trimmed);
        // Recipe declaration: "name:" or "name arg:" or "name arg='default':"
        if let Some(colon) = recipe.find(':') {
            let before = &recipe[..colon];
            let name = before.split_whitespace().next().unwrap_or("");
            if !name.is_empty()
                && !name.starts_with('_')
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                ctx.tasks.push(Task {
                    name: name.to_string(),
                    source: TaskSource::Justfile,
                });
            }
        }
    }
}

fn extract_taskfile_tasks(dir: &Path, ctx: &mut ProjectContext) {
    let path = first_existing(dir, &["Taskfile.yml", "Taskfile.yaml", "taskfile.yml"]);
    let Some(content) = path.and_then(|p| fs::read_to_string(p).ok()) else {
        return;
    };
    // Lightweight YAML parse: look for top-level keys under "tasks:"
    // Lines like "  taskname:" after a "tasks:" line
    let mut in_tasks = false;
    for line in content.lines() {
        if line.trim() == "tasks:" {
            in_tasks = true;
            continue;
        }
        if in_tasks {
            // End of tasks block: line is a top-level key (no leading space) or empty
            if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
                break;
            }
            // Task name: exactly 2-space or 1-tab indent, then "name:"
            let stripped = line.strip_prefix("  ").or_else(|| line.strip_prefix('\t'));
            if let Some(rest) = stripped
                && !rest.starts_with(' ')
                && !rest.starts_with('\t')
                && let Some(colon) = rest.find(':')
            {
                let name = rest[..colon].trim();
                if !name.is_empty()
                    && !name.starts_with('#')
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    ctx.tasks.push(Task {
                        name: name.to_string(),
                        source: TaskSource::Taskfile,
                    });
                }
            }
        }
    }
}

fn extract_deno_tasks(dir: &Path, ctx: &mut ProjectContext) {
    let path = first_existing(dir, &["deno.json", "deno.jsonc"]);
    let Some(p) = path else { return };
    let Ok(content) = fs::read_to_string(p) else {
        return;
    };
    // deno.jsonc may contain comments — serde_json will fail on those, which is fine
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let Ok(d) = serde_json::from_str::<Partial>(&content) else {
        return;
    };
    if let Some(tasks) = d.tasks {
        for name in tasks.keys() {
            ctx.tasks.push(Task {
                name: name.clone(),
                source: TaskSource::DenoJson,
            });
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn exists(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}

fn any_exists(dir: &Path, names: &[&str]) -> bool {
    names.iter().any(|n| exists(dir, n))
}

fn first_existing(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|n| dir.join(n)).find(|p| p.exists())
}

fn read_trimmed(dir: &Path, name: &str) -> Option<String> {
    fs::read_to_string(dir.join(name)).ok()
}
