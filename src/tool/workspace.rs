//! Workspace member discovery from root declarations.
//!
//! Reads every declaration present at the root (`pnpm-workspace.yaml`,
//! `package.json` `"workspaces"`, `lerna.json`, `deno.json` `"workspace"`,
//! `Cargo.toml` `[workspace]`), expands the globs, and keeps each directory
//! that carries the manifest its declaration implies.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use yaml_rust2::YamlLoader;

use crate::tool::{deno, files, node};
use crate::types::{Workspace, WorkspaceKind, WorkspaceMember};

/// Discover the workspace declared at `root`, or `None` when no
/// declaration exists.
pub(crate) fn discover(root: &Path) -> Option<Workspace> {
    let mut kinds = Vec::new();
    let mut members: Vec<Arc<WorkspaceMember>> = Vec::new();
    for (kind, globs) in declarations(root) {
        kinds.push(kind);
        for dir in expand(root, &globs) {
            if !has_manifest(kind, &dir) || members.iter().any(|member| member.dir == dir) {
                continue;
            }
            members.push(Arc::new(member(root, kind, dir)));
        }
    }
    if kinds.is_empty() {
        return None;
    }
    members.sort_by(|a, b| a.path.cmp(&b.path));
    Some(Workspace {
        root: root.to_path_buf(),
        kinds,
        members,
        current: None,
    })
}

/// The workspace `dir` belongs to: the workspace declared in `dir` itself,
/// else the nearest ancestor (inside the VCS root) declaring one. When
/// `dir` sits inside one of that workspace's members, `current` names it.
/// A directory that carries its own project files without being a member
/// (a nested standalone project, a test fixture) is not anchored.
pub(crate) fn anchor(dir: &Path) -> Option<Workspace> {
    if let Some(workspace) = discover(dir) {
        return Some(workspace);
    }
    let standalone = has_own_project_files(dir);
    files::find_in_ancestors(dir, |ancestor| {
        if ancestor == dir {
            return None;
        }
        let mut workspace = discover(ancestor)?;
        workspace.current = workspace
            .members
            .iter()
            .find(|member| dir.starts_with(&member.dir))
            .cloned();
        if workspace.current.is_none() && standalone {
            return None;
        }
        Some(workspace)
    })
}

/// Whether `dir` holds a manifest or task-runner config of its own.
fn has_own_project_files(dir: &Path) -> bool {
    const STANDALONE: &[&str] = &[
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "nx.json",
        "runner.toml",
        "pnpm-workspace.yaml",
        "lerna.json",
    ];
    node::has_package_json(dir)
        || [
            STANDALONE,
            deno::FILENAMES,
            crate::tool::turbo::FILENAMES,
            crate::tool::make::FILENAMES,
            crate::tool::just::FILENAMES,
            crate::tool::go_task::FILENAMES,
            crate::tool::mise::FILENAMES,
            crate::tool::bacon::FILENAMES,
        ]
        .iter()
        .any(|names| files::find_first(dir, names).is_some())
}

fn declarations(root: &Path) -> Vec<(WorkspaceKind, Vec<String>)> {
    let mut out = Vec::new();
    if let Some(globs) = pnpm_globs(root) {
        out.push((WorkspaceKind::PnpmWorkspace, globs));
    }
    if let Some(globs) = node::workspace_globs(root) {
        out.push((WorkspaceKind::PackageJson, globs));
    }
    if let Some(globs) = lerna_globs(root) {
        out.push((WorkspaceKind::Lerna, globs));
    }
    if let Some(globs) = deno_globs(root) {
        out.push((WorkspaceKind::DenoJson, globs));
    }
    if let Some(globs) = cargo_globs(root) {
        out.push((WorkspaceKind::Cargo, globs));
    }
    out
}

fn pnpm_globs(root: &Path) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(root.join("pnpm-workspace.yaml")).ok()?;
    let docs = YamlLoader::load_from_str(&content).ok()?;
    let packages = docs
        .first()?
        .as_hash()?
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some("packages")).then_some(value))?;
    Some(
        packages
            .as_vec()?
            .iter()
            .filter_map(yaml_rust2::Yaml::as_str)
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn lerna_globs(root: &Path) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Lerna {
        #[serde(default)]
        packages: Option<Vec<String>>,
        #[serde(default)]
        use_workspaces: bool,
    }
    let content = std::fs::read_to_string(root.join("lerna.json")).ok()?;
    let lerna = serde_json::from_str::<Lerna>(&content).ok()?;
    if lerna.use_workspaces {
        return None;
    }
    Some(
        lerna
            .packages
            .unwrap_or_else(|| vec!["packages/*".to_string()]),
    )
}

fn deno_globs(root: &Path) -> Option<Vec<String>> {
    let config = files::find_first(root, deno::FILENAMES).filter(|path| path.is_file())?;
    deno::workspace_patterns(&config)
}

#[derive(Deserialize)]
struct CargoManifest {
    package: Option<CargoPackage>,
    workspace: Option<CargoWorkspace>,
}

#[derive(Deserialize)]
struct CargoPackage {
    name: String,
}

#[derive(Deserialize)]
struct CargoWorkspace {
    #[serde(default)]
    members: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn cargo_manifest(dir: &Path) -> Option<CargoManifest> {
    let content = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    toml::from_str(&content).ok()
}

fn cargo_globs(root: &Path) -> Option<Vec<String>> {
    let workspace = cargo_manifest(root)?.workspace?;
    Some(
        workspace
            .members
            .into_iter()
            .chain(workspace.exclude.into_iter().map(|glob| format!("!{glob}")))
            .collect(),
    )
}

fn has_manifest(kind: WorkspaceKind, dir: &Path) -> bool {
    match kind {
        WorkspaceKind::PackageJson | WorkspaceKind::PnpmWorkspace | WorkspaceKind::Lerna => {
            node::has_package_json(dir)
        }
        WorkspaceKind::DenoJson => {
            files::find_first(dir, deno::FILENAMES).is_some() || node::has_package_json(dir)
        }
        WorkspaceKind::Cargo => dir.join("Cargo.toml").is_file(),
    }
}

fn member(root: &Path, kind: WorkspaceKind, dir: PathBuf) -> WorkspaceMember {
    let declared = match kind {
        WorkspaceKind::PackageJson | WorkspaceKind::PnpmWorkspace | WorkspaceKind::Lerna => {
            node::manifest_name(&dir)
        }
        WorkspaceKind::DenoJson => deno::config_name(&dir).or_else(|| node::manifest_name(&dir)),
        WorkspaceKind::Cargo => cargo_manifest(&dir).and_then(|m| m.package).map(|p| p.name),
    };
    let path = relative_path(root, &dir);
    let name = declared.filter(|name| !name.is_empty()).unwrap_or_else(|| {
        dir.file_name()
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned())
    });
    WorkspaceMember { name, path, dir }
}

fn relative_path(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: true,
};

/// Expand `globs` (with `!` negations) to existing directories under
/// `root`, skipping anything inside `node_modules`.
fn expand(root: &Path, globs: &[String]) -> Vec<PathBuf> {
    let (negatives, positives): (Vec<&str>, Vec<&str>) = globs
        .iter()
        .map(String::as_str)
        .partition(|glob| glob.starts_with('!'));
    let negatives: Vec<glob::Pattern> = negatives
        .iter()
        .filter_map(|glob| glob::Pattern::new(&normalize(&glob[1..])).ok())
        .collect();
    let escaped_root = glob::Pattern::escape(&root.to_string_lossy());

    let mut dirs: Vec<PathBuf> = Vec::new();
    for positive in positives {
        let pattern = normalize(positive);
        if pattern.is_empty() {
            continue;
        }
        let Ok(paths) = glob::glob_with(&format!("{escaped_root}/{pattern}"), MATCH_OPTIONS) else {
            continue;
        };
        for path in paths.filter_map(Result::ok) {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let inside_node_modules = relative
                .components()
                .any(|component| component.as_os_str() == "node_modules");
            let excluded = negatives
                .iter()
                .any(|negative| negative.matches_path_with(relative, MATCH_OPTIONS));
            if !path.is_dir() || inside_node_modules || excluded || dirs.contains(&path) {
                continue;
            }
            dirs.push(path);
        }
    }
    dirs
}

fn normalize(glob: &str) -> String {
    glob.trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::discover;
    use crate::tool::test_support::TempDir;
    use crate::types::WorkspaceKind;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("file has a parent"))
            .expect("parent dir should be created");
        fs::write(path, content).expect("file should be written");
    }

    fn names(workspace: &crate::types::Workspace) -> Vec<(&str, &str)> {
        workspace
            .members
            .iter()
            .map(|member| (member.name.as_str(), member.path.as_str()))
            .collect()
    }

    #[test]
    fn no_declaration_means_no_workspace() {
        let dir = TempDir::new("workspace-none");
        write(dir.path(), "package.json", r#"{ "name": "solo" }"#);

        assert!(discover(dir.path()).is_none());
    }

    #[test]
    fn package_json_workspaces_expand_globs_and_negations() {
        let dir = TempDir::new("workspace-npm");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": ["packages/*", "rfc", "!packages/legacy"] }"#,
        );
        write(
            dir.path(),
            "packages/web/package.json",
            r#"{ "name": "@acme/web" }"#,
        );
        write(
            dir.path(),
            "packages/api/package.json",
            r#"{ "name": "api" }"#,
        );
        write(
            dir.path(),
            "packages/legacy/package.json",
            r#"{ "name": "legacy" }"#,
        );
        write(dir.path(), "packages/notes/README.md", "no manifest here");
        write(dir.path(), "rfc/package.json", "{}");
        write(
            dir.path(),
            "node_modules/packages/x/package.json",
            r#"{ "name": "x" }"#,
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(workspace.kinds, vec![WorkspaceKind::PackageJson]);
        assert_eq!(
            names(&workspace),
            vec![
                ("api", "packages/api"),
                ("@acme/web", "packages/web"),
                ("rfc", "rfc"),
            ],
        );
    }

    #[test]
    fn yarn_object_form_is_accepted() {
        let dir = TempDir::new("workspace-yarn");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": { "packages": ["apps/*"], "nohoist": ["**/react"] } }"#,
        );
        write(
            dir.path(),
            "apps/site/package.json",
            r#"{ "name": "site" }"#,
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(names(&workspace), vec![("site", "apps/site")]);
    }

    #[test]
    fn pnpm_workspace_yaml_lists_packages() {
        let dir = TempDir::new("workspace-pnpm");
        write(
            dir.path(),
            "pnpm-workspace.yaml",
            "packages:\n  - 'packages/**'\n  - '!**/test/**'\n",
        );
        write(dir.path(), "package.json", "{}");
        write(dir.path(), "packages/a/package.json", r#"{ "name": "a" }"#);
        write(
            dir.path(),
            "packages/a/test/fixture/package.json",
            r#"{ "name": "fixture" }"#,
        );
        write(
            dir.path(),
            "packages/group/b/package.json",
            r#"{ "name": "b" }"#,
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(workspace.kinds, vec![WorkspaceKind::PnpmWorkspace]);
        assert_eq!(
            names(&workspace),
            vec![("a", "packages/a"), ("b", "packages/group/b")],
        );
    }

    #[test]
    fn cargo_workspace_members_honor_exclude() {
        let dir = TempDir::new("workspace-cargo");
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/skip\"]\n",
        );
        write(
            dir.path(),
            "crates/core/Cargo.toml",
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\n",
        );
        write(
            dir.path(),
            "crates/skip/Cargo.toml",
            "[package]\nname = \"skip\"\nversion = \"0.1.0\"\n",
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(workspace.kinds, vec![WorkspaceKind::Cargo]);
        assert_eq!(names(&workspace), vec![("core-lib", "crates/core")]);
    }

    #[test]
    fn deno_workspace_members_are_discovered() {
        let dir = TempDir::new("workspace-deno");
        write(
            dir.path(),
            "deno.json",
            r#"{ "workspace": ["./members/*"] }"#,
        );
        write(
            dir.path(),
            "members/lib/deno.json",
            r#"{ "name": "@scope/lib", "tasks": { "check": "deno check" } }"#,
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(workspace.kinds, vec![WorkspaceKind::DenoJson]);
        assert_eq!(names(&workspace), vec![("@scope/lib", "members/lib")]);
    }

    #[test]
    fn mixed_declarations_merge_members_by_directory() {
        let dir = TempDir::new("workspace-mixed");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": ["packages/*"] }"#,
        );
        write(
            dir.path(),
            "Cargo.toml",
            "[workspace]\nmembers = [\"packages/*\", \"crates/*\"]\n",
        );
        write(
            dir.path(),
            "packages/both/package.json",
            r#"{ "name": "both-js" }"#,
        );
        write(
            dir.path(),
            "packages/both/Cargo.toml",
            "[package]\nname = \"both-rs\"\nversion = \"0.1.0\"\n",
        );
        write(
            dir.path(),
            "crates/only/Cargo.toml",
            "[package]\nname = \"only\"\nversion = \"0.1.0\"\n",
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(
            workspace.kinds,
            vec![WorkspaceKind::PackageJson, WorkspaceKind::Cargo],
        );
        assert_eq!(
            names(&workspace),
            vec![("only", "crates/only"), ("both-js", "packages/both")],
        );
    }

    #[test]
    fn resolve_prefers_exact_name_or_path_over_directory_name() {
        let dir = TempDir::new("workspace-resolve");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": ["apps/*", "tools/*"] }"#,
        );
        write(
            dir.path(),
            "apps/web/package.json",
            r#"{ "name": "@acme/web" }"#,
        );
        write(dir.path(), "tools/web/package.json", r#"{ "name": "web" }"#);

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        let by_name: Vec<_> = workspace
            .resolve("web")
            .into_iter()
            .map(|m| m.path.as_str())
            .collect();
        assert_eq!(by_name, vec!["tools/web"]);

        let by_path: Vec<_> = workspace
            .resolve("apps/web")
            .into_iter()
            .map(|m| m.path.as_str())
            .collect();
        assert_eq!(by_path, vec!["apps/web"]);

        assert!(workspace.resolve("nope").is_empty());
    }

    #[test]
    fn anchor_finds_the_workspace_from_any_directory_beneath_it() {
        let dir = TempDir::new("workspace-anchor");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": ["packages/*"] }"#,
        );
        write(
            dir.path(),
            "packages/web/package.json",
            r#"{ "name": "web" }"#,
        );
        write(dir.path(), "packages/web/src/index.ts", "");
        write(dir.path(), "docs/README.md", "");

        let from_root = super::anchor(dir.path()).expect("root declares the workspace");
        assert_eq!(from_root.root, dir.path());
        assert!(from_root.current.is_none());

        let member_dir = dir.path().join("packages/web");
        let from_member = super::anchor(&member_dir).expect("member sits in the workspace");
        assert_eq!(from_member.root, dir.path());
        assert_eq!(
            from_member.current.as_ref().map(|m| m.name.as_str()),
            Some("web")
        );

        let from_nested = super::anchor(&member_dir.join("src")).expect("nested dir");
        assert_eq!(from_nested.root, dir.path());
        assert_eq!(
            from_nested.current.as_ref().map(|m| m.name.as_str()),
            Some("web")
        );

        let from_docs = super::anchor(&dir.path().join("docs")).expect("non-member dir");
        assert_eq!(from_docs.root, dir.path());
        assert!(from_docs.current.is_none());
    }

    #[test]
    fn anchor_leaves_a_nested_standalone_project_alone() {
        let dir = TempDir::new("workspace-anchor-standalone");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": ["packages/*"] }"#,
        );
        write(
            dir.path(),
            "packages/web/package.json",
            r#"{ "name": "web" }"#,
        );
        write(
            dir.path(),
            "tests/fixtures/chain/justfile",
            "build:\n\techo hi\n",
        );
        write(
            dir.path(),
            "examples/demo/package.json",
            r#"{ "name": "demo" }"#,
        );

        assert!(super::anchor(&dir.path().join("tests/fixtures/chain")).is_none());
        assert!(super::anchor(&dir.path().join("examples/demo")).is_none());
        assert!(
            super::anchor(&dir.path().join("tests/fixtures")).is_some(),
            "a directory without project files of its own still anchors",
        );
    }

    #[test]
    fn anchor_stops_at_the_vcs_root() {
        let outer = TempDir::new("workspace-anchor-boundary");
        write(
            outer.path(),
            "package.json",
            r#"{ "workspaces": ["repo/*"] }"#,
        );
        let repo = outer.path().join("repo");
        fs::create_dir_all(repo.join(".git")).expect("git dir should be created");
        write(&repo, "lib/package.json", r#"{ "name": "lib" }"#);

        assert!(super::anchor(&repo.join("lib")).is_none());
    }

    #[test]
    fn directory_name_is_ambiguous_only_without_an_exact_match() {
        let dir = TempDir::new("workspace-ambiguous");
        write(
            dir.path(),
            "package.json",
            r#"{ "workspaces": ["apps/*", "tools/*"] }"#,
        );
        write(
            dir.path(),
            "apps/web/package.json",
            r#"{ "name": "@acme/app-web" }"#,
        );
        write(
            dir.path(),
            "tools/web/package.json",
            r#"{ "name": "@acme/tool-web" }"#,
        );

        let workspace = discover(dir.path()).expect("workspace should be discovered");

        assert_eq!(workspace.resolve("web").len(), 2);
    }
}
