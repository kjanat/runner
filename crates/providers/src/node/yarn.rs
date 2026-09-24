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
            locked_only_with: None,
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
        locked_only_with: None,
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
) -> Vec<runner_core::Evidence> {
    let mut derived = Vec::new();
    for item in evidence
        .iter()
        .filter(|e| e.provider == Some(ProviderId::Yarn))
    {
        let dir = match &item.scope {
            runner_core::Scope::Root => &tree.root,
            runner_core::Scope::Member { dir, .. } => dir,
        };
        let declared_berry = matches!(&item.declared, Some(Declared::Version(version)) if version.split('.').next().and_then(|v| v.parse::<u32>().ok()).is_some_and(|major| major >= 2));
        let manifest_berry = std::fs::read_to_string(dir.join("package.json")).ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|manifest| manifest.get("packageManager").and_then(package_manager))
            .is_some_and(|declared| matches!(declared, Declared::Version(version) if version.split('.').next().and_then(|v| v.parse::<u32>().ok()).is_some_and(|major| major >= 2)));
        let lock_berry = std::fs::read_to_string(dir.join("yarn.lock"))
            .is_ok_and(|text| text.lines().any(|line| line == "__metadata:"));
        if declared_berry || manifest_berry || dir.join(".yarnrc.yml").is_file() || lock_berry {
            let mut variant = item.clone();
            variant.declared = Some(Declared::Variant("berry".into()));
            if !derived.contains(&variant) {
                derived.push(variant);
            }
        }
    }
    derived
}
