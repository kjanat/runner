//! Cargo.

use runner_core::{
    Capabilities, CleanCap, Discovery, Ecosystem, Frozen, Hooks, InstallCap, Kind, Provider,
    ProviderId, QuietSupport, RunTaskCap, ScriptSupport, Signal, TestCap, t,
};

/// Cargo.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Cargo,
    label: "cargo",
    aliases: &["cargo-alias"],
    ecosystem: Ecosystem::Rust,
    kind: Kind::PACKAGE_MANAGER.union(Kind::TASK_SOURCE),
    program: Some("cargo"),
    signals: &[
        Signal::File("Cargo.toml"),
        Signal::Lockfile("Cargo.lock"),
        Signal::Probe("cargo"),
    ],
    writes: &["target"],
    caps: Capabilities {
        task_priority: 3,
        install: Some(InstallCap {
            argv: t!["fetch", Frozen],
            frozen: Frozen::Flag("--locked"),
            scripts: ScriptSupport::NONE,
            locked_only_with: &[],
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, Task, Args],
            sources: &[ProviderId::Cargo],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["test", Args],
            discovery: Discovery::Tool,
        }),
        clean: Some(CleanCap {
            dir_suffixes: &[],
            framework_dirs: &[],
            dirs: &["target"],
        }),
        quiet: QuietSupport::flag(t!["-q"]),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::cargo_aliases::tasks),
    version: None,
    hooks: Hooks::NONE,
};
