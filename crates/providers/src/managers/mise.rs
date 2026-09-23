//! mise.

use runner_core::{
    Capabilities, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind, NameShape, Provider,
    ProviderId, QuietSupport, Reach, RunTaskCap, ScriptSupport, Signal, t,
};

/// The default operation when `[tools.mise].install` says nothing.
pub const INSTALL: &str = "install";

/// mise.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Mise,
    label: "mise",
    aliases: &["rtx", "mise.toml", ".mise.toml"],
    ecosystem: Ecosystem::Any,
    kind: Kind::TASK_SOURCE.union(Kind::TOOL_MANAGER),
    program: Some("mise"),
    signals: &[
        Signal::FileUpwards("mise.local.toml"),
        Signal::FileUpwards("mise.toml"),
        Signal::FileUpwards(".mise.local.toml"),
        Signal::FileUpwards(".mise.toml"),
        Signal::FileUpwards("mise/config.toml"),
        Signal::FileUpwards(".mise/config.toml"),
        Signal::FileUpwards(".config/mise.toml"),
        Signal::FileUpwards(".config/mise/config.toml"),
        Signal::EnvVar("MISE_SHELL"),
        Signal::Probe("mise"),
    ],
    writes: &[],
    caps: Capabilities {
        install: Some(InstallCap {
            argv: t![Quiet, Op, Frozen],
            frozen: Frozen::Flag("--locked"),
            scripts: ScriptSupport::NONE,
            locked_only_with: Some("mise.lock"),
        }),
        run_task: Some(RunTaskCap {
            argv: t![Quiet, "run", Task, Sep("--"), Args],
            sources: &[ProviderId::Mise],
        }),
        exec: Some(ExecCap {
            program: None,
            argv: t!["exec", "--", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        operations: &[INSTALL, "bootstrap"],
        quiet: QuietSupport::flag(t!["--quiet"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
