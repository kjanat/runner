//! Pipenv.

use runner_core::{
    BinDirs, BinsCap, Capabilities, Discovery, Ecosystem, Frozen, Hooks, InstallCap, Kind,
    Provider, ProviderId, QuietSupport, RunTaskCap, ScriptSupport, Signal, TestCap, t,
};

/// Pipenv.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Pipenv,
    label: "pipenv",
    aliases: &[],
    ecosystem: Ecosystem::Python,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("pipenv"),
    signals: &[
        Signal::File("Pipfile"),
        Signal::Lockfile("Pipfile.lock"),
        Signal::Probe("pipenv"),
    ],
    caps: Capabilities {
        writes: &[".venv"],
        install: Some(InstallCap {
            argv: t!["install"],
            frozen: Frozen::Argv(t!["sync"]),
            scripts: ScriptSupport::NONE,
            locked_only_with: &[],
            lockfiles: None,
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, "run", Task, Args],
            sources: &[ProviderId::Pyproject],
        }),
        test: Some(TestCap {
            program: None,
            argv: t!["run", Args],
            discovery: Discovery::Detect(super::test_runner),
            file_flags: None,
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
