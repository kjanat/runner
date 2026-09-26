//! npm.

use runner_core::{
    Capabilities, Declared, Ecosystem, ExecCap, Field, Frozen, Hooks, InstallCap, Kind, Lockfiles,
    NameShape, Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptMechanism,
    ScriptSupport, Signal, WorkspaceCap, t,
};

use super::manifest;

fn package_manager(field: &Field<'_>) -> Option<Declared> {
    manifest::package_manager(field, ProviderId::Npm)
}

fn dev_engines(field: &Field<'_>) -> Option<Declared> {
    manifest::dev_engines(field, ProviderId::Npm)
}

const MANIFEST: [Signal; 2] = super::manifest_signals(package_manager, dev_engines);

/// npm.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Npm,
    label: "npm",
    aliases: &["npx"],
    ecosystem: Ecosystem::Node,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("npm"),
    signals: &[
        Signal::Lockfile("package-lock.json"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::Probe("npm"),
    ],
    caps: Capabilities {
        writes: super::WRITES,
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
            lockfiles: Some(Lockfiles::Named(&["npm-shrinkwrap.json"])),
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
            declarations: super::workspace::declarations,
        }),
        clean: Some(super::CLEAN),
        quiet: QuietSupport::flag(t!["--silent"]),
        runs_on: Some(ProviderId::Node),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks {
        before_plan: Some(manifest::before_plan),
        ..Hooks::NONE
    },
};
