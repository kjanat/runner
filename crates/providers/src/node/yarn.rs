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
        variants: &[("berry", BERRY)],
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
    },
    tasks: None,
    version: None,
    hooks: Hooks {
        after_observe: Some(after_observe),
        ..Hooks::NONE
    },
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
        let manifest = dir.join("package.json");
        let manifest_berry = match read_optional(&manifest)? {
            Some(text) => {
                let value: Value = serde_json::from_str(&text).map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{}: {error}", manifest.display()),
                    )
                })?;
                value
                    .get("packageManager")
                    .and_then(package_manager)
                    .as_ref()
                    .is_some_and(is_berry)
            }
            None => false,
        };
        let lock = dir.join("yarn.lock");
        let lock_berry = read_optional(&lock)?
            .is_some_and(|text| text.lines().any(|line| line == "__metadata:"));
        let config = dir.join(".yarnrc.yml");
        let configured = match std::fs::metadata(&config) {
            Ok(metadata) => metadata.is_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(at_path(&config, &error)),
        };
        let declaration = evidence.iter().find(|e| {
            e.provider == item.provider
                && e.scope == item.scope
                && e.declared.as_ref().is_some_and(is_berry)
        });
        if declaration.is_some() || manifest_berry || configured || lock_berry {
            let mut variant = item.clone();
            variant.at = declaration.map_or_else(
                || {
                    if manifest_berry {
                        manifest
                    } else if configured {
                        config
                    } else {
                        lock
                    }
                },
                |declaration| declaration.at.clone(),
            );
            variant.declared = Some(Declared::Variant("berry".into()));
            derived.push(variant);
        }
    }
    Ok(derived)
}

fn is_berry(declared: &Declared) -> bool {
    let Declared::Version(version) = declared else {
        return false;
    };
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 2)
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
