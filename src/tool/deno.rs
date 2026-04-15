//! Deno — secure JavaScript/TypeScript runtime.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

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

/// Parse task names from `deno.json` / `deno.jsonc`.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
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
    Ok(d.tasks.map_or_else(Vec::new, |t| t.into_keys().collect()))
}

/// `deno task <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("task").arg(task).args(args);
    c
}

/// `deno install`
pub(crate) fn install_cmd() -> Command {
    let mut c = Command::new("deno");
    c.arg("install");
    c
}

/// `deno run <args...>`
pub(crate) fn exec_cmd(args: &[String]) -> Command {
    let mut c = Command::new("deno");
    c.arg("run").args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{detect, extract_tasks, find_config_upwards, workspace_pattern_matches};
    use crate::tool::test_support::TempDir;

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

        let mut tasks = extract_tasks(dir.path()).expect("deno tasks should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "test"]);
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
