//! Workspace discovery and anchoring through the real providers.

use std::fs;
use std::path::Path;

use runner_core::workspace::{Workspace, anchor, discover};
use runner_providers::REGISTRY;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("file has a parent")).expect("parent dir");
    fs::write(path, content).expect("file");
}

fn found(root: &Path) -> Workspace {
    discover(root, &REGISTRY)
        .expect("declarations read")
        .expect("a workspace is declared")
}

fn names(workspace: &Workspace) -> Vec<(&str, &str)> {
    workspace
        .members
        .iter()
        .map(|member| (member.name.as_str(), member.path.as_str()))
        .collect()
}

fn labels(workspace: &Workspace) -> Vec<&str> {
    workspace
        .members
        .iter()
        .map(|member| member.label.as_str())
        .collect()
}

#[test]
fn no_declaration_means_no_workspace() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "package.json", r#"{ "name": "solo" }"#);
    assert_eq!(discover(dir.path(), &REGISTRY).unwrap(), None);
}

#[test]
fn package_json_workspaces_expand_globs_and_negations() {
    let dir = tempfile::tempdir().unwrap();
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

    let workspace = found(dir.path());
    assert_eq!(workspace.kinds, ["package.json workspaces"]);
    assert_eq!(
        names(&workspace),
        [
            ("api", "packages/api"),
            ("@acme/web", "packages/web"),
            ("rfc", "rfc"),
        ]
    );
}

#[test]
fn yarn_object_form_and_package_yaml_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{ "workspaces": { "packages": ["apps/*"], "nohoist": ["**/react"] } }"#,
    );
    write(dir.path(), "apps/site/package.yaml", "name: site\n");
    assert_eq!(names(&found(dir.path())), [("site", "apps/site")]);
}

#[test]
fn pnpm_workspace_yaml_lists_packages() {
    let dir = tempfile::tempdir().unwrap();
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

    let workspace = found(dir.path());
    assert_eq!(workspace.kinds, ["pnpm-workspace.yaml"]);
    assert_eq!(
        names(&workspace),
        [("a", "packages/a"), ("b", "packages/group/b")]
    );
}

#[test]
fn cargo_workspace_members_honor_exclude() {
    let dir = tempfile::tempdir().unwrap();
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

    let workspace = found(dir.path());
    assert_eq!(workspace.kinds, ["Cargo.toml workspace"]);
    assert_eq!(names(&workspace), [("core-lib", "crates/core")]);
}

#[test]
fn deno_workspace_members_are_discovered() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "deno.jsonc",
        "{ // members\n \"workspace\": [\"./members/*\"] }",
    );
    write(
        dir.path(),
        "members/lib/deno.json",
        r#"{ "name": "@scope/lib", "tasks": { "check": "deno check" } }"#,
    );

    let workspace = found(dir.path());
    assert_eq!(workspace.kinds, ["deno.json workspace"]);
    assert_eq!(names(&workspace), [("@scope/lib", "members/lib")]);
}

#[test]
fn mixed_declarations_merge_members_by_directory() {
    let dir = tempfile::tempdir().unwrap();
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

    let workspace = found(dir.path());
    assert_eq!(
        workspace.kinds,
        ["package.json workspaces", "Cargo.toml workspace"]
    );
    assert_eq!(
        names(&workspace),
        [("only", "crates/only"), ("both-js", "packages/both")]
    );
}

#[test]
fn members_sharing_a_name_are_labeled_by_path() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{ "workspaces": ["apps/*", "tools/*"] }"#,
    );
    write(dir.path(), "apps/docs/package.json", "{}");
    write(dir.path(), "apps/web/package.json", "{}");
    write(dir.path(), "tools/web/package.json", "{}");
    write(
        dir.path(),
        "tools/cli/package.json",
        r#"{ "name": "@acme/cli" }"#,
    );

    let workspace = found(dir.path());
    assert_eq!(
        labels(&workspace),
        ["docs", "apps/web", "@acme/cli", "tools/web"]
    );
}

#[test]
fn anchor_finds_the_workspace_from_any_directory_beneath_it() {
    let dir = tempfile::tempdir().unwrap();
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
    let anchored = |at: &Path| {
        anchor(at, Some(dir.path()), false, &REGISTRY)
            .unwrap()
            .expect("anchored")
    };

    let from_root = anchored(dir.path());
    assert_eq!(from_root.root, dir.path());
    assert_eq!(from_root.current(), None);
    for at in [
        dir.path().join("packages/web"),
        dir.path().join("packages/web/src"),
    ] {
        let from_member = anchored(&at);
        assert_eq!(from_member.root, dir.path());
        assert_eq!(from_member.current().map(|m| m.name.as_str()), Some("web"));
    }
    assert_eq!(anchored(&dir.path().join("docs")).current(), None);
}

#[test]
fn anchor_leaves_a_standalone_directory_outside_every_member_alone() {
    let dir = tempfile::tempdir().unwrap();
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
        "examples/demo/package.json",
        r#"{ "name": "demo" }"#,
    );
    let demo = dir.path().join("examples/demo");
    assert_eq!(
        anchor(&demo, Some(dir.path()), true, &REGISTRY).unwrap(),
        None
    );
}

#[test]
fn anchor_stops_at_the_boundary() {
    let outer = tempfile::tempdir().unwrap();
    write(
        outer.path(),
        "package.json",
        r#"{ "workspaces": ["repo/*"] }"#,
    );
    let repo = outer.path().join("repo");
    write(&repo, "lib/package.json", r#"{ "name": "lib" }"#);
    assert_eq!(
        anchor(&repo.join("lib"), Some(&repo), false, &REGISTRY).unwrap(),
        None
    );
}
