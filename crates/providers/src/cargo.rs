//! Cargo.

use std::path::Path;

use crate::workspace::Manifest;

use runner_core::{
    Capabilities, CleanCap, Discovery, Ecosystem, Frozen, Hooks, InstallCap, Kind, Provider,
    ProviderId, QuietSupport, RunTaskCap, ScriptSupport, Signal, TestCap, t,
};

/// The members `Cargo.toml` `[workspace]` declares at `root`, each carrying
/// a `Cargo.toml`, less its `exclude`.
fn declarations(root: &Path) -> Result<Vec<runner_core::Declaration>, runner_core::Warning> {
    let Some(manifest) = manifest(root)? else {
        return Ok(Vec::new());
    };
    let Some(workspace) = manifest.get("workspace") else {
        return Ok(Vec::new());
    };
    let list = |key: &str| {
        workspace
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let globs: Vec<String> = list("members")
        .into_iter()
        .chain(list("exclude").into_iter().map(|glob| format!("!{glob}")))
        .collect();
    Ok(vec![runner_core::Declaration {
        kind: "Cargo.toml workspace",
        members: crate::workspace::members(root, &globs, member_name)?,
    }])
}

fn manifest(dir: &Path) -> Result<Option<toml::Table>, runner_core::Warning> {
    let path = dir.join("Cargo.toml");
    crate::workspace::read(&path)?
        .map(|text| {
            text.parse::<toml::Table>().map_err(|error| {
                runner_core::Warning::general(format!("{}: {error}", path.display()))
            })
        })
        .transpose()
}

fn member_name(dir: &Path) -> Result<Manifest, runner_core::Warning> {
    Ok(manifest(dir)?.map_or(Manifest::Absent, |manifest| {
        Manifest::named(
            manifest
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str),
        )
    }))
}

/// Cargo.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Cargo,
    label: "cargo",
    aliases: &["cargo-alias"],
    ecosystem: Ecosystem::Rust,
    kind: Kind::PACKAGE_MANAGER.union(Kind::TASK_SOURCE),
    program: Some("cargo"),
    signals: &[
        Signal::File("Cargo.toml"),
        Signal::Lockfile("Cargo.lock"),
        Signal::Probe("cargo"),
    ],
    caps: Capabilities {
        writes: &["target"],
        task_table: runner_core::TaskTable::Key("alias"),
        task_priority: 6,
        workspaces: Some(runner_core::WorkspaceCap { declarations }),
        install: Some(InstallCap {
            argv: t!["fetch", Frozen],
            frozen: Frozen::Flag("--locked"),
            scripts: ScriptSupport::NONE,
            locked_only_with: &[],
            lockfiles: None,
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, Task, Args],
            sources: &[ProviderId::Cargo],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["test", Args],
            discovery: Discovery::Tool,
            file_flags: None,
        }),
        clean: Some(CleanCap {
            dir_suffixes: &[],
            framework_dirs: &[],
            dirs: &["target"],
        }),
        quiet: QuietSupport::flag(t!["-q"]),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::cargo_aliases::tasks),
    version: None,
    hooks: Hooks::NONE,
};
