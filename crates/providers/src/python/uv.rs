//! uv.

use runner_core::{
    BinDirs, BinsCap, Capabilities, Discovery, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind,
    NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap, RunTaskCap, ScriptSupport,
    Signal, TestCap, t,
};

/// uv.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Uv,
    label: "uv",
    aliases: &[],
    ecosystem: Ecosystem::Python,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("uv"),
    signals: &[Signal::Lockfile("uv.lock"), Signal::Probe("uv")],
    writes: &[".venv"],
    caps: Capabilities {
        package_exec: Some(ExecCap {
            program: Some("uvx"),
            argv: t!["--from", Package, Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        install: Some(InstallCap {
            argv: t!["sync", Frozen],
            frozen: Frozen::Flag("--frozen"),
            scripts: ScriptSupport::NONE,
            locked_only_with: &[],
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, "run", Task, Args],
            sources: &[ProviderId::Pyproject],
        }),
        exec: Some(ExecCap {
            program: Some("uvx"),
            argv: t![Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
        }),
        run_file: Some(RunFileCap {
            unsupported: &[],
            program: None,
            extensions: &["py"],
            argv: t!["run", File, Args],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["run", Args],
            discovery: Discovery::Detect(super::test_runner),
        }),
        bins: Some(BinsCap {
            dirs: BinDirs::Ask(super::venv::bin_dirs),
        }),
        clean: Some(super::CLEAN),
        quiet: QuietSupport::flag(t!["--quiet"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
