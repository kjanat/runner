//! Yarn. Declared as Classic; Berry's `--immutable`, `YARN_ENABLE_SCRIPTS` and missing `--silent` arrive with the `after_observe` hook.

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
    hooks: Hooks::NONE,
};
