//! Deno — secure JavaScript/TypeScript runtime.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use super::ScriptDirective;
use crate::tool::files;
use crate::tool::node;

/// Directories produced by Deno.
pub(crate) const CLEAN_DIRS: &[&str] = &[".deno"];

/// Supported Deno config filenames (priority order).
pub(crate) const FILENAMES: &[&str] = &["deno.json", "deno.jsonc"];

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkspaceField {
    Members(Vec<String>),
    Config { members: Vec<String> },
}

#[derive(Deserialize)]
struct WorkspaceConfig {
    workspace: Option<WorkspaceField>,
}

/// Resolve the nearest supported Deno config while walking upward.
pub(crate) fn find_config_upwards(dir: &Path) -> Option<PathBuf> {
    let boundary = vcs_root(dir);

    for ancestor in dir.ancestors() {
        if !within_boundary(ancestor, boundary.as_deref()) {
            break;
        }

        let Some(path) = files::find_first(ancestor, FILENAMES).filter(|path| path.is_file())
        else {
            continue;
        };

        if ancestor == dir || workspace_includes_dir(&path, dir) {
            return Some(path);
        }

        return None;
    }

    None
}

/// Detected via `deno.json`, `deno.jsonc`, `deno.lock`, or `packageManager:
/// deno@...` in a supported package manifest.
pub(crate) fn detect(dir: &Path) -> bool {
    find_config_upwards(dir).is_some()
        || dir.join("deno.lock").exists()
        || detect_pm_from_field_upwards(dir)
            .is_some_and(|pm| pm == crate::types::PackageManager::Deno)
}

fn detect_pm_from_field_upwards(dir: &Path) -> Option<crate::types::PackageManager> {
    let boundary = vcs_root(dir);

    for ancestor in dir.ancestors() {
        if !within_boundary(ancestor, boundary.as_deref()) {
            break;
        }

        if let Some(path) = files::find_first(ancestor, FILENAMES).filter(|path| path.is_file())
            && ancestor != dir
            && !workspace_includes_dir(&path, dir)
        {
            return None;
        }

        if let Some(pm) = node::detect_pm_from_field(ancestor) {
            return Some(pm);
        }
    }

    None
}

fn workspace_includes_dir(config_path: &Path, dir: &Path) -> bool {
    let Some(patterns) = workspace_patterns(config_path) else {
        return true;
    };

    let Some(config_dir) = config_path.parent() else {
        return false;
    };

    if dir == config_dir {
        return true;
    }

    dir.ancestors()
        .take_while(|ancestor| *ancestor != config_dir)
        .filter_map(|ancestor| ancestor.strip_prefix(config_dir).ok())
        .any(|relative| {
            patterns
                .iter()
                .any(|pattern| workspace_pattern_matches(pattern, relative))
        })
}

fn workspace_patterns(config_path: &Path) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(config_path).ok()?;
    let config = json5::from_str::<WorkspaceConfig>(&content).ok()?;

    match config.workspace? {
        WorkspaceField::Members(members) | WorkspaceField::Config { members } => Some(members),
    }
}

fn workspace_pattern_matches(pattern: &str, relative: &Path) -> bool {
    let pattern = normalize_workspace_pattern(pattern);
    let path = path_segments(relative);

    pattern.len() == path.len()
        && pattern
            .iter()
            .zip(path.iter())
            .all(|(expected, actual)| expected == "*" || expected == actual)
}

fn normalize_workspace_pattern(pattern: &str) -> Vec<String> {
    Path::new(pattern)
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn path_segments(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn vcs_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|ancestor| ancestor.join(".jj").is_dir() || ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn within_boundary(path: &Path, boundary: Option<&Path>) -> bool {
    boundary.is_none_or(|boundary| path == boundary || path.starts_with(boundary))
}

/// Parse task names and descriptions from `deno.json` / `deno.jsonc`.
///
/// Handles both the string form (`"build": "vite build"`) and the object
/// form (`"build": { "command": "...", "description": "..." }`). Sorted
/// by name for deterministic output. The self-exec path re-parses the
/// config for `command` / `dependencies` when it needs them.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<(String, Option<String>)>> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let Some(path) = find_config_upwards(dir) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let d = json5::from_str::<Partial>(&content)
        .with_context(|| format!("{} is not valid JSON/JSONC", path.display()))?;
    let mut tasks: Vec<(String, Option<String>)> = d.tasks.map_or_else(Vec::new, |t| {
        t.into_iter()
            .map(|(name, value)| {
                // String form carries no description; object form may.
                let description = value
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                (name, description)
            })
            .collect()
    });
    tasks.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(tasks)
}

/// `deno task <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("deno");
    c.arg("task").arg(task).args(args);
    c
}

/// `deno x <args...>` — Deno's `npx`-equivalent (Deno 2.x+).
///
/// Resolves and runs an `npm:` / `jsr:` package's binary entry point
/// without installing it permanently. Bare-name targets (no registry
/// prefix) fail at Deno's side; the runner passes the user's
/// `--pm deno` intent through verbatim rather than second-guessing.
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = super::program::command("deno");
    c.arg("x").args(args);
    c
}

/// `deno install [--allow-scripts]`
///
/// Deno denies all npm lifecycle scripts by default, so
/// [`ScriptDirective::Deny`]/[`ScriptDirective::Default`] add nothing.
/// [`ScriptDirective::ForceOn`] appends a bare `--allow-scripts`, which Deno
/// reads as "allow every package" (its flag takes `0..` values; bare = all),
/// running all npm lifecycle scripts.
pub(crate) fn install_cmd(scripts: ScriptDirective) -> Command {
    let mut c = super::program::command("deno");
    c.arg("install");
    if scripts == ScriptDirective::ForceOn {
        c.arg("--allow-scripts");
    }
    c
}

/// `deno run <file> [args...]` — execute a local source file with the
/// Deno runtime. Distinct from [`exec_cmd`] (`deno x`), which resolves a
/// remote `npm:`/`jsr:` package; this runs an on-disk path. No extra
/// permission flags are added — a script that needs them should carry a
/// `#!/usr/bin/env -S deno run -A`-style shebang.
pub(crate) fn run_file_cmd(file: &Path, args: &[String]) -> Command {
    let mut c = super::program::command("deno");
    c.arg("run").arg(file).args(args);
    c
}

/// Whether this Deno project materializes a local `node_modules/` — its
/// `nodeModulesDir` resolves to `auto`/`manual` (Deno 2.x) or the legacy
/// boolean `true`. When it does, `deno install` writes the same directory a
/// node-ecosystem PM (`npm`/`yarn`/`pnpm`/`bun`) would, so installing with
/// both is a collision. Unreadable/absent config or any other value
/// (`none`, unset) means Deno keeps deps in its global cache — no collision.
pub(crate) fn writes_node_modules(dir: &Path) -> bool {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(rename = "nodeModulesDir")]
        node_modules_dir: Option<serde_json::Value>,
    }
    let Some(path) = find_config_upwards(dir) else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(parsed) = json5::from_str::<Partial>(&content) else {
        return false;
    };
    match parsed.node_modules_dir {
        Some(serde_json::Value::Bool(enabled)) => enabled,
        Some(serde_json::Value::String(mode)) => matches!(mode.as_str(), "auto" | "manual"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        ScriptDirective, detect, exec_cmd, extract_tasks, find_config_upwards, install_cmd,
        run_file_cmd, workspace_pattern_matches,
    };
    use crate::tool::test_support::TempDir;

    fn install_args(scripts: ScriptDirective) -> Vec<String> {
        install_cmd(scripts)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn install_denies_by_default_without_flag() {
        // Deno denies all npm lifecycle scripts by default, so deny/default
        // need no flag.
        assert_eq!(install_args(ScriptDirective::Default), ["install"]);
        assert_eq!(install_args(ScriptDirective::Deny), ["install"]);
    }

    #[test]
    fn install_force_on_allows_all_scripts() {
        // Bare `--allow-scripts` allows every package's lifecycle scripts.
        assert_eq!(
            install_args(ScriptDirective::ForceOn),
            ["install", "--allow-scripts"]
        );
    }

    #[test]
    fn run_file_cmd_uses_deno_run_with_file() {
        // A local `.ts`/`.js` source file dispatches as `deno run <file>`,
        // never `deno x <file>` (the registry-package path).
        let args = [String::from("--port"), String::from("8080")];
        let cmd = run_file_cmd(Path::new("/abs/server.ts"), &args);
        let built: Vec<_> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), "deno");
        assert_eq!(built, ["run", "/abs/server.ts", "--port", "8080"]);
    }

    #[test]
    fn exec_uses_deno_x_passthrough() {
        // `runner --pm deno run npm:create-vite my-app` should build
        // `deno x npm:create-vite my-app` — the `x` subcommand sits
        // before the target so Deno's `npx`-equivalent picks up the
        // user's verbatim args.
        let args = [String::from("npm:create-vite"), String::from("my-app")];
        let built: Vec<_> = exec_cmd(&args)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["x", "npm:create-vite", "my-app"]);
    }

    #[test]
    fn writes_node_modules_reads_node_modules_dir() {
        use super::writes_node_modules;
        let cases = [
            (r#"{ "nodeModulesDir": "auto" }"#, true),
            (r#"{ "nodeModulesDir": "manual" }"#, true),
            (r#"{ "nodeModulesDir": "none" }"#, false),
            (r#"{ "nodeModulesDir": true }"#, true),
            (r#"{ "nodeModulesDir": false }"#, false),
            (r#"{ "tasks": {} }"#, false), // unset
            (r"{ /* jsonc */ }", false),
        ];
        for (i, (body, expected)) in cases.iter().enumerate() {
            let dir = TempDir::new(&format!("deno-nmd-{i}"));
            fs::write(dir.path().join("deno.json"), body).expect("write config");
            assert_eq!(writes_node_modules(dir.path()), *expected, "body: {body}");
        }
    }

    #[test]
    fn writes_node_modules_false_without_config() {
        use super::writes_node_modules;
        let dir = TempDir::new("deno-no-config");
        assert!(!writes_node_modules(dir.path()));
    }

    #[test]
    fn extract_tasks_supports_jsonc_comments_and_trailing_commas() {
        let dir = TempDir::new("deno-jsonc");

        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{
  // line comment
  "tasks": {
    "build": "deno task build",
    /* block comment */
    "test": "deno test",
  },
}
"#,
        )
        .expect("deno.jsonc should be written");

        let tasks = extract_tasks(dir.path()).expect("deno tasks should parse");

        assert_eq!(
            tasks,
            [("build".to_string(), None), ("test".to_string(), None),]
        );
    }

    #[test]
    fn extract_tasks_reads_object_form_descriptions() {
        let dir = TempDir::new("deno-task-descriptions");
        fs::write(
            dir.path().join("deno.json"),
            r#"{
  "tasks": {
    "build": { "command": "vite build", "description": "Bundle for production" },
    "dev": "vite"
  }
}"#,
        )
        .expect("deno.json should be written");

        let tasks = extract_tasks(dir.path()).expect("deno tasks should parse");

        assert_eq!(
            tasks,
            [
                (
                    "build".to_string(),
                    Some("Bundle for production".to_string())
                ),
                ("dev".to_string(), None),
            ]
        );
    }

    #[test]
    fn detect_supports_package_manager_field() {
        let dir = TempDir::new("deno-package-manager-field");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "deno@2.7.12" }"#,
        )
        .expect("package.json should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn detect_supports_deno_lock() {
        let dir = TempDir::new("deno-lock-detect");
        fs::write(dir.path().join("deno.lock"), "{}").expect("deno.lock should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn find_config_upwards_prefers_nearest_config() {
        let dir = TempDir::new("deno-config-upwards");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.jsonc"),
            "{ tasks: { root: 'deno task root' } }",
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("deno.json"),
            r#"{ "tasks": { "member": "deno task member" } }"#,
        )
        .expect("member deno.json should be written");

        let path = find_config_upwards(&nested).expect("nearest config should resolve");

        assert!(path.ends_with("apps/site/deno.json"));
    }

    #[test]
    fn detect_does_not_leak_parent_deno_into_git_repo() {
        let outer = TempDir::new("deno-detect-boundary-outer");
        let repo = outer.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("git dir should be created");
        fs::write(outer.path().join("deno.lock"), "{}").expect("outer deno.lock should be written");

        assert!(!detect(&repo));
    }

    #[test]
    fn find_config_upwards_stops_when_workspace_excludes_path() {
        let dir = TempDir::new("deno-config-workspace-excluded");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.json"),
            r#"{ "workspace": ["./packages/*"] }"#,
        )
        .expect("root deno.json should be written");

        assert_eq!(find_config_upwards(&nested), None);
    }

    #[test]
    fn find_config_upwards_accepts_workspace_member_paths() {
        let dir = TempDir::new("deno-config-workspace-member");
        let nested = dir.path().join("packages").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            dir.path().join("deno.json"),
            r#"{ "workspace": { "members": ["packages/*"] } }"#,
        )
        .expect("root deno.json should be written");

        let path = find_config_upwards(&nested).expect("workspace member should resolve");

        assert!(path.ends_with("deno.json"));
    }

    #[test]
    fn workspace_pattern_matches_single_level_glob() {
        assert!(workspace_pattern_matches(
            "packages/*",
            Path::new("packages/site")
        ));
        assert!(!workspace_pattern_matches(
            "packages/*",
            Path::new("packages/site/src"),
        ));
    }
}
