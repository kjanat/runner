//! mise.

use runner_core::{
    BinDirs, BinsCap, Capabilities, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind, NameShape,
    Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptSupport, Signal, t,
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
            locked_only_with: &[
                ("mise.toml", "mise.lock"),
                (".mise.toml", "mise.lock"),
                ("mise.local.toml", "mise.local.lock"),
                (".mise.local.toml", "mise.local.lock"),
                ("mise/config.toml", "mise/mise.lock"),
                (".mise/config.toml", ".mise/mise.lock"),
                (".config/mise.toml", ".config/mise.lock"),
                (".config/mise/config.toml", ".config/mise/mise.lock"),
            ],
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
        bins: Some(BinsCap {
            dirs: BinDirs::Ask(bin_paths),
        }),
        operations: &[INSTALL, "bootstrap"],
        quiet: QuietSupport::flag(t!["--quiet"]),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};

fn bin_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let program = runner_core::probe_with("mise", &[]).unwrap_or_else(|| "mise".into());
    let Ok(output) = std::process::Command::new(program)
        .arg("bin-paths")
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}
