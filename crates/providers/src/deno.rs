//! Deno.

use runner_core::{
    Capabilities, CleanCap, Declared, Discovery, Ecosystem, ExecCap, Frozen, Hooks, InstallCap,
    Kind, NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap, RunTaskCap,
    ScriptMechanism, ScriptSupport, Signal, TestCap, t,
};
use serde_json::Value;

use crate::node::manifest;

fn package_manager(value: &Value) -> Option<Declared> {
    manifest::package_manager(value, ProviderId::Deno)
}

fn dev_engines(value: &Value) -> Option<Declared> {
    manifest::dev_engines(value, ProviderId::Deno)
}

const MANIFEST: [Signal; 2] = crate::node::manifest_signals(package_manager, dev_engines);

/// Deno.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Deno,
    label: "deno",
    aliases: &[],
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
        install: Some(InstallCap {
            argv: t!["install", Frozen, Scripts],
            frozen: Frozen::Flag("--frozen"),
            scripts: ScriptSupport {
                deny: ScriptMechanism::Default,
                allow: ScriptMechanism::Flag("--allow-scripts"),
            },
            locked_only_with: None,
        }),
        run_task: Some(RunTaskCap {
            argv: t!["task", Quiet, Task, Args],
            sources: &[ProviderId::Deno, ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: None,
            argv: t!["x", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
        }),
        run_file: Some(RunFileCap {
            program: None,
            extensions: crate::node::bun::SCRIPT_EXTENSIONS,
            argv: t!["run", File, Args],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["test", Args],
            discovery: Discovery::Tool,
        }),
        clean: Some(CleanCap { dirs: &[".deno"] }),
        quiet: QuietSupport::flag(t!["-q"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
