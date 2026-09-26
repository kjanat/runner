//! mise.

use runner_core::{
    BinDirs, BinsCap, Capabilities, Ecosystem, ExecCap, Frozen, HealthCap, Hooks, InstallCap, Kind,
    NameShape, Provider, ProviderId, QuietSupport, Reach, RunTaskCap, ScriptSupport, Signal, t,
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
    caps: Capabilities {
        task_table: runner_core::TaskTable::Key("tasks"),
        task_priority: 9,
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
            lockfiles: None,
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
        health: &[
            HealthCap {
                argv: t!["ls", "--missing", "--json"],
                parse: crate::extract::mise::parse_missing_health,
            },
            HealthCap {
                argv: t!["tasks", "validate", "--json"],
                parse: crate::extract::mise::parse_task_health,
            },
        ],
        usage: Some(runner_core::UsageCap {
            spec: crate::extract::mise::usage,
        }),
        operations: &[INSTALL, "bootstrap"],
        quiet: QuietSupport::flag(t!["--quiet"]),
        ..Capabilities::NONE
    },
    tasks: Some(crate::extract::mise::tasks),
    version: None,
    hooks: Hooks {
        before_plan: Some(before_plan),
        after_observe: None,
    },
};

/// Refuse a task whose spec marks a flag required when that flag is absent,
/// before mise starts the task and whatever it builds.
fn before_plan(
    tree: &runner_core::Tree,
    present: &runner_core::Present,
    op: &runner_core::Op<'_>,
    _: &runner_core::Policy,
    warnings: &mut Vec<runner_core::Warning>,
) -> Result<(), runner_core::Refusal> {
    let runner_core::Op::Run { task, args } = op else {
        return Ok(());
    };
    if task.source != ProviderId::Mise {
        return Ok(());
    }
    let spec = match crate::extract::mise::usage(tree, present, task) {
        Ok(Some(spec)) => spec,
        Ok(None) => return Ok(()),
        Err(mut warning) => {
            warning
                .message
                .push_str("; required flags were not checked");
            warnings.push(warning);
            return Ok(());
        }
    };
    let missing = spec.missing_required_flags(args);
    if missing.is_empty() {
        return Ok(());
    }
    Err(runner_core::Refusal::Invalid(format!(
        "{} requires {}\n  usage: {} {}",
        task.name,
        missing.join(", "),
        task.name,
        spec.signature,
    )))
}

fn bin_paths(root: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let Some(program) = runner_core::probe_with("mise", &[]) else {
        return Ok(Vec::new());
    };
    let output = std::process::Command::new(program)
        .arg("bin-paths")
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "mise bin-paths failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(std::path::PathBuf::from)
        .collect())
}
