//! Yarn Classic and Berry, distinguished by observed project evidence.

use runner_core::{
    Capabilities, Declared, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind, NameShape,
    Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptMechanism, ScriptSupport, Signal,
    WorkspaceCap, t,
};
use serde_json::Value;

use super::manifest;

fn package_manager(value: &Value) -> Option<Declared> {
    manifest::package_manager(value, ProviderId::Yarn)
}

fn dev_engines(value: &Value) -> Option<Declared> {
    manifest::dev_engines(value, ProviderId::Yarn)
}

const MANIFEST: [Signal; 2] = super::manifest_signals(package_manager, dev_engines);

/// Yarn.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Yarn,
    label: "yarn",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("yarn"),
    signals: &[
        Signal::Lockfile("yarn.lock"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::Probe("yarn"),
    ],
    writes: super::WRITES,
    caps: Capabilities {
        probe_priority: 3,
        variants: &[("classic", CLASSIC), ("berry", BERRY)],
        install: Some(InstallCap {
            argv: t!["install", Frozen, Scripts],
            frozen: Frozen::Flag("--frozen-lockfile"),
            scripts: ScriptSupport {
                deny: ScriptMechanism::FlagAndEnv(
                    "--ignore-scripts",
                    "YARN_ENABLE_SCRIPTS",
                    "false",
                ),
                allow: ScriptMechanism::Default,
            },
            locked_only_with: &[],
        }),
        ..CLASSIC
    },
    tasks: None,
    version: None,
    hooks: Hooks {
        before_plan: Some(manifest::before_plan),
        after_observe: Some(after_observe),
    },
};

const CLASSIC: Capabilities = Capabilities {
    install: Some(InstallCap {
        argv: t!["install", Frozen, Scripts],
        frozen: Frozen::Flag("--frozen-lockfile"),
        scripts: ScriptSupport {
            deny: ScriptMechanism::Flag("--ignore-scripts"),
            allow: ScriptMechanism::Default,
        },
        locked_only_with: &[],
    }),
    run_task: Some(RunTaskCap {
        argv: t![Quiet, Task, Args],
        sources: &[ProviderId::PackageJson],
    }),
    exec: Some(ExecCap {
        program: None,
        argv: t!["run", Name, Args],
        reach: Reach::Local,
        accepts: NameShape::BARE,
    }),
    test: Some(super::TEST),
    bins: Some(super::BINS),
    workspaces: Some(WorkspaceCap {
        members: super::workspace::members,
    }),
    clean: Some(super::CLEAN),
    quiet: QuietSupport::flag(t!["--silent"]),
    ..Capabilities::NONE
};

const BERRY: Capabilities = Capabilities {
    package_exec: Some(ExecCap {
        program: None,
        argv: t!["dlx", "--package", Package, Name, Args],
        reach: Reach::Network,
        accepts: NameShape::BARE,
    }),
    exec: Some(ExecCap {
        program: None,
        argv: t!["exec", Name, Args],
        reach: Reach::Local,
        accepts: NameShape::BARE,
    }),
    install: Some(InstallCap {
        argv: t!["install", Frozen, Scripts],
        frozen: Frozen::Flag("--immutable"),
        scripts: ScriptSupport {
            deny: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "false"),
            allow: ScriptMechanism::Env("YARN_ENABLE_SCRIPTS", "true"),
        },
        locked_only_with: &[],
    }),
    run_task: Some(RunTaskCap {
        argv: t!["run", Task, Args],
        sources: &[ProviderId::PackageJson],
    }),
    quiet: QuietSupport::unsupported("--silent is Yarn Classic-only"),
    variants: &[],
    test: Some(super::TEST),
    bins: Some(super::BINS),
    workspaces: Some(WorkspaceCap {
        members: super::workspace::members,
    }),
    clean: Some(super::CLEAN),
    ..Capabilities::NONE
};

fn after_observe(
    tree: &runner_core::Tree,
    evidence: &[runner_core::Evidence],
) -> std::io::Result<Vec<runner_core::Evidence>> {
    let mut derived = Vec::new();
    let mut scopes = Vec::new();
    for item in evidence
        .iter()
        .filter(|e| e.provider == Some(ProviderId::Yarn))
    {
        if scopes.contains(&item.scope) {
            continue;
        }
        scopes.push(item.scope.clone());
        let dir = match &item.scope {
            runner_core::Scope::Root => &tree.root,
            runner_core::Scope::Member { dir, .. } => dir,
        };
        let Some((at, name)) = variant_of(dir, evidence, item)? else {
            continue;
        };
        let mut variant = item.clone();
        variant.at = at;
        variant.declared = Some(Declared::Variant(name.into()));
        derived.push(variant);
    }
    Ok(derived)
}

/// The Yarn line `dir` uses and the file that says so, strongest evidence first.
///
/// Every source is read before any is trusted, so a broken file is an error
/// even when a stronger source answers.
fn variant_of(
    dir: &std::path::Path,
    evidence: &[runner_core::Evidence],
    item: &runner_core::Evidence,
) -> std::io::Result<Option<(std::path::PathBuf, &'static str)>> {
    let manifest = dir.join("package.json");
    let from_manifest = match read_optional(&manifest)? {
        Some(text) => serde_json::from_str::<Value>(&text)
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {error}", manifest.display()),
                )
            })?
            .get("packageManager")
            .and_then(package_manager)
            .as_ref()
            .and_then(line),
        None => None,
    };
    let config = dir.join(".yarnrc.yml");
    let configured = match std::fs::metadata(&config) {
        Ok(metadata) => metadata.is_file(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(at_path(&config, &error)),
    };
    let lock = dir.join("yarn.lock");
    let from_lock = read_optional(&lock)?.and_then(|text| {
        if text.lines().any(|line| line == "__metadata:") {
            Some("berry")
        } else {
            text.lines()
                .any(|line| line == "# yarn lockfile v1")
                .then_some("classic")
        }
    });
    let declared = evidence.iter().find_map(|e| {
        (e.provider == item.provider && e.scope == item.scope)
            .then(|| e.declared.as_ref().and_then(line))
            .flatten()
            .map(|line| (e.at.clone(), line))
    });
    Ok(declared
        .or_else(|| from_manifest.map(|line| (manifest, line)))
        .or_else(|| configured.then_some((config, "berry")))
        .or_else(|| from_lock.map(|line| (lock, line))))
}

/// `classic` for a Yarn 1 version, `berry` for 2 and later.
fn line(declared: &Declared) -> Option<&'static str> {
    let major = declared.version()?.split('.').next()?.parse::<u32>().ok()?;
    Some(if major >= 2 { "berry" } else { "classic" })
}

fn at_path(path: &std::path::Path, error: &std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn read_optional(path: &std::path::Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(at_path(path, &error)),
    }
}
