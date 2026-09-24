//! pnpm.

use runner_core::{
    Capabilities, Declared, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind, NameShape,
    Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptMechanism, ScriptSupport, Signal,
    WorkspaceCap, t,
};
use serde_json::Value;

use super::manifest;

fn package_manager(value: &Value) -> Option<Declared> {
    manifest::package_manager(value, ProviderId::Pnpm)
}

fn dev_engines(value: &Value) -> Option<Declared> {
    manifest::dev_engines(value, ProviderId::Pnpm)
}

const MANIFEST: [Signal; 2] = super::manifest_signals(package_manager, dev_engines);

/// pnpm.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Pnpm,
    label: "pnpm",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("pnpm"),
    signals: &[
        Signal::Lockfile("pnpm-lock.yaml"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::Probe("pnpm"),
    ],
    writes: super::WRITES,
    caps: Capabilities {
        package_exec: Some(ExecCap {
            program: None,
            argv: runner_core::Template(&[
                runner_core::Piece::Concat(&[
                    runner_core::Piece::Lit("--package="),
                    runner_core::Piece::Package,
                ]),
                runner_core::Piece::Lit("dlx"),
                runner_core::Piece::Name,
                runner_core::Piece::Args,
            ]),
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        install: Some(InstallCap {
            argv: t!["install", Frozen, Scripts],
            frozen: Frozen::Flag("--frozen-lockfile"),
            scripts: ScriptSupport {
                deny: ScriptMechanism::Flag("--ignore-scripts"),
                allow: ScriptMechanism::Warn("pnpm.onlyBuiltDependencies"),
            },
            locked_only_with: &[],
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, "run", Task, Sep("--"), Args],
            sources: &[ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: None,
            argv: t!["exec", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
        }),
        test: Some(super::TEST),
        bins: Some(super::BINS),
        workspaces: Some(WorkspaceCap {
            members: super::workspace::members,
        }),
        clean: Some(super::CLEAN),
        quiet: QuietSupport::flag_with_stream(t!["--silent"], t!["--use-stderr"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
