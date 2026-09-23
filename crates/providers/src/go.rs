//! Go modules.

use runner_core::{
    Capabilities, CleanCap, Discovery, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind,
    NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap, RunTaskCap, ScriptSupport,
    Signal, TestCap, t,
};

/// Go.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Go,
    label: "go",
    aliases: &[],
    ecosystem: Ecosystem::Go,
    kind: Kind::PACKAGE_MANAGER.union(Kind::TASK_SOURCE),
    program: Some("go"),
    signals: &[
        Signal::File("go.mod"),
        Signal::Lockfile("go.sum"),
        Signal::Probe("go"),
    ],
    writes: &["vendor"],
    caps: Capabilities {
        install: Some(InstallCap {
            argv: t!["mod", "download"],
            frozen: Frozen::Unsupported,
            scripts: ScriptSupport::NONE,
            locked_only_with: None,
        }),
        run_task: Some(RunTaskCap {
            argv: t!["run", Task, Args],
            sources: &[ProviderId::Go],
        }),
        exec: Some(ExecCap {
            program: None,
            argv: t!["run", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::PATH_LIKE.union(NameShape::VERSIONED),
        }),
        run_file: Some(RunFileCap {
            program: None,
            extensions: &["go"],
            argv: t!["run", File, Args],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["test", "./...", Args],
            discovery: Discovery::Tool,
        }),
        clean: Some(CleanCap { dirs: &["vendor"] }),
        quiet: QuietSupport::unsupported("go run has no host-only quiet mode"),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
