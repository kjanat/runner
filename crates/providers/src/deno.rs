//! Deno.

use std::path::Path;

use crate::workspace::Manifest;

use runner_core::{
    Capabilities, CleanCap, Declared, Discovery, Ecosystem, ExecCap, Field, Frozen, Hooks,
    InstallCap, Kind, Lockfiles, NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap,
    RunTaskCap, ScriptMechanism, ScriptSupport, Signal, TestCap, t,
};

use crate::node::manifest;

fn package_manager(field: &Field<'_>) -> Option<Declared> {
    manifest::package_manager(field, ProviderId::Deno)
}

fn dev_engines(field: &Field<'_>) -> Option<Declared> {
    manifest::dev_engines(field, ProviderId::Deno)
}

const MANIFEST: [Signal; 2] = crate::node::manifest_signals(package_manager, dev_engines);

const CONFIGS: [&str; 2] = ["deno.json", "deno.jsonc"];

/// The Deno config in `dir` as a JSON value, `None` when there is none.
fn config(dir: &Path) -> Result<Option<serde_json::Value>, runner_core::Warning> {
    for name in CONFIGS {
        let path = dir.join(name);
        if let Some(text) = crate::workspace::read(&path)? {
            return json5::from_str(&text).map(Some).map_err(|error| {
                runner_core::Warning::general(format!("{}: {error}", path.display()))
            });
        }
    }
    Ok(None)
}

/// The workspace a Deno config's `"workspace"` declares at `root`, each member
/// carrying a Deno config or a package manifest.
fn declarations(root: &Path) -> Result<Vec<runner_core::Declaration>, runner_core::Warning> {
    let globs = config(root)?.and_then(|config| match &config["workspace"] {
        serde_json::Value::Array(list) => Some(list.clone()),
        serde_json::Value::Object(map) => map
            .get("members")
            .and_then(serde_json::Value::as_array)
            .cloned(),
        _ => None,
    });
    let Some(globs) = globs else {
        return Ok(Vec::new());
    };
    let globs: Vec<String> = globs
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .collect();
    Ok(vec![runner_core::Declaration {
        kind: "deno.json workspace",
        members: crate::workspace::members(root, &globs, member_name)?,
    }])
}

fn member_name(dir: &Path) -> Result<Manifest, runner_core::Warning> {
    let name = |document: &serde_json::Value| Manifest::named(document["name"].as_str());
    if let Some(config) = config(dir)? {
        return Ok(name(&config));
    }
    Ok(runner_core::read_manifest(dir, crate::node::MANIFESTS)
        .map_err(|error| runner_core::Warning::general(error.to_string()))?
        .map_or(Manifest::Absent, |(_, document)| name(&document)))
}

/// The lockfile the nearest Deno config names with `"lock"`, relative to that config.
fn lockfiles(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    #[derive(serde::Deserialize)]
    struct Config {
        lock: Option<serde_json::Value>,
    }
    let Some(config) = dir.ancestors().find_map(|ancestor| {
        ["deno.json", "deno.jsonc"]
            .into_iter()
            .map(|name| ancestor.join(name))
            .find(|path| path.is_file())
    }) else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(&config)?;
    let parsed = json5::from_str::<Config>(&text).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: {error}", config.display()),
        )
    })?;
    let path = match &parsed.lock {
        Some(serde_json::Value::String(path)) => Some(path.as_str()),
        Some(serde_json::Value::Object(lock)) => {
            lock.get("path").and_then(serde_json::Value::as_str)
        }
        _ => None,
    };
    Ok(path
        .zip(config.parent())
        .map(|(path, base)| vec![base.join(path)])
        .unwrap_or_default())
}

/// Deno.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Deno,
    label: "deno",
    aliases: &["deno.json", "deno.jsonc"],
    ecosystem: Ecosystem::Deno,
    kind: Kind::PACKAGE_MANAGER
        .union(Kind::TASK_SOURCE)
        .union(Kind::RUNTIME),
    program: Some("deno"),
    signals: &[
        Signal::FileUpwards("deno.json"),
        Signal::FileUpwards("deno.jsonc"),
        Signal::Lockfile("deno.lock"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::Probe("deno"),
    ],
    writes: crate::node::WRITES,
    caps: Capabilities {
        task_priority: 5,
        probe_priority: 4,
        package_exec: Some(ExecCap {
            program: None,
            argv: runner_core::Template(&[
                runner_core::Piece::Lit("x"),
                runner_core::Piece::Concat(&[
                    runner_core::Piece::Lit("npm:"),
                    runner_core::Piece::Package,
                    runner_core::Piece::Lit("/"),
                    runner_core::Piece::Name,
                ]),
                runner_core::Piece::Args,
            ]),
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        file_interpreters: &["node", "nodejs", "bun", "deno"],
        install: Some(InstallCap {
            argv: t!["install", Frozen, Scripts],
            frozen: Frozen::Flag("--frozen"),
            scripts: ScriptSupport {
                deny: ScriptMechanism::Default,
                allow: ScriptMechanism::Flag("--allow-scripts"),
            },
            locked_only_with: &[],
            lockfiles: Some(Lockfiles::Ask(lockfiles)),
        }),
        run_task: Some(RunTaskCap {
            argv: t!["task", Quiet, Task, Args],
            sources: &[ProviderId::Deno, ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: None,
            argv: t!["x", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE
                .union(NameShape::VERSIONED)
                .union(NameShape::REGISTRY),
        }),
        run_file: Some(RunFileCap {
            unsupported: &[],
            program: None,
            extensions: crate::node::bun::SCRIPT_EXTENSIONS,
            argv: t!["run", File, Args],
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
            dirs: &[".deno"],
        }),
        workspaces: Some(runner_core::WorkspaceCap { declarations }),
        quiet: QuietSupport::flag(t!["-q"]),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::scripts::deno_tasks),
    version: None,
    hooks: Hooks {
        before_plan: Some(manifest::before_plan),
        ..Hooks::NONE
    },
};
