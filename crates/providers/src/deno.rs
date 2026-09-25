//! Deno.

use runner_core::{
    Capabilities, CleanCap, Declared, Discovery, Ecosystem, ExecCap, Field, Frozen, Hooks,
    InstallCap, Kind, NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap, RunTaskCap,
    ScriptMechanism, ScriptSupport, Signal, TestCap, t,
};

use crate::node::manifest;

fn package_manager(field: &Field<'_>) -> Option<Declared> {
    manifest::package_manager(field, ProviderId::Deno)
}

fn dev_engines(field: &Field<'_>) -> Option<Declared> {
    manifest::dev_engines(field, ProviderId::Deno)
}

const MANIFEST: [Signal; 2] = crate::node::manifest_signals(package_manager, dev_engines);

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
