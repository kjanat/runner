//! Bun.

use runner_core::{
    Capabilities, Declared, Discovery, Ecosystem, ExecCap, Field, Frozen, Hooks, InstallCap, Kind,
    NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap, RunTaskCap, RuntimeCap,
    ScriptMechanism, ScriptSupport, Signal, TestCap, WorkspaceCap, t,
};

use super::manifest;

fn package_manager(field: &Field<'_>) -> Option<Declared> {
    manifest::package_manager(field, ProviderId::Bun)
}

fn dev_engines(field: &Field<'_>) -> Option<Declared> {
    manifest::dev_engines(field, ProviderId::Bun)
}

const MANIFEST: [Signal; 2] = super::manifest_signals(package_manager, dev_engines);

/// Source extensions the JavaScript runtimes run.
pub const SCRIPT_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "ts", "mts", "cts", "jsx", "tsx"];

/// Bun.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Bun,
    label: "bun",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::PACKAGE_MANAGER.union(Kind::RUNTIME),
    program: Some("bun"),
    signals: &[
        Signal::Lockfile("bun.lock"),
        Signal::Lockfile("bun.lockb"),
        MANIFEST[0],
        MANIFEST[1],
        Signal::Probe("bun"),
    ],
    caps: Capabilities {
        writes: super::WRITES,
        probe_priority: 1,
        package_exec: Some(ExecCap {
            program: None,
            argv: t!["x", "--package", Package, Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        file_interpreters: &["node", "nodejs", "bun", "deno"],
        install: Some(InstallCap {
            argv: t!["install", Frozen, Scripts],
            frozen: Frozen::Flag("--frozen-lockfile"),
            scripts: ScriptSupport {
                deny: ScriptMechanism::Flag("--ignore-scripts"),
                allow: ScriptMechanism::Warn("trustedDependencies"),
            },
            locked_only_with: &[],
            lockfiles: None,
        }),
        run_task: Some(RunTaskCap {
            argv: t!["run", Quiet, Task, Args],
            sources: &[ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: None,
            argv: t!["x", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
        }),
        run_file: Some(RunFileCap {
            unsupported: &[],
            program: None,
            extensions: SCRIPT_EXTENSIONS,
            argv: t![File, Args],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["test", Args],
            discovery: Discovery::Tool,
            file_flags: None,
        }),
        bins: Some(super::BINS),
        workspaces: Some(WorkspaceCap {
            declarations: super::workspace::declarations,
        }),
        clean: Some(super::CLEAN),
        as_runtime: Some(RuntimeCap {
            package_exec: Some(t!["x", "--bun", "--package", Package, Name, Args]),
            run_task: Some(t!["--bun", "run", Quiet, Task, Args]),
            exec: Some(t!["x", "--bun", Name, Args]),
        }),
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
