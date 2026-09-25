//! npm.

use runner_core::{
    Capabilities, Declared, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind, NameShape,
    Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptMechanism, ScriptSupport, Signal,
    WorkspaceCap, t,
};
use serde_json::Value;

use super::manifest;

fn package_manager(value: &Value) -> Option<Declared> {
    manifest::package_manager(value, ProviderId::Npm)
}

fn dev_engines(value: &Value) -> Option<Declared> {
    manifest::dev_engines(value, ProviderId::Npm)
}

const MANIFEST: [Signal; 2] = super::manifest_signals(package_manager, dev_engines);

/// npm.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Npm,
    label: "npm",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("npm"),
    signals: &[
        Signal::Lockfile("package-lock.json"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::Probe("npm"),
    ],
    writes: super::WRITES,
    caps: Capabilities {
        package_exec: Some(ExecCap {
            program: Some("npx"),
            argv: t!["--package", Package, "--", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        install: Some(InstallCap {
            argv: t!["install", Scripts],
            frozen: Frozen::Argv(t!["ci", Scripts]),
            scripts: ScriptSupport {
                deny: ScriptMechanism::Flag("--ignore-scripts"),
                allow: ScriptMechanism::Flag("--no-ignore-scripts"),
            },
            locked_only_with: &[],
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, "run", Task, Sep("--"), Args],
            sources: &[ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: Some("npx"),
            argv: t![Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
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
        before_plan: Some(manifest::before_plan),
        ..Hooks::NONE
    },
};
