//! Node.js as a runtime.

use runner_core::{
    Capabilities, Ecosystem, ExecCap, Hooks, Kind, NameShape, Provider, ProviderId, QuietSupport,
    Reach, RunFileCap, RunTaskCap, Signal, t,
};

use super::manifest::engines_node;

/// Node.js.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Node,
    label: "node",
    aliases: &[],
    ecosystem: Ecosystem::Node,
    kind: Kind::RUNTIME,
    program: Some("node"),
    signals: &[
        Signal::File(".nvmrc"),
        Signal::File(".node-version"),
        Signal::ManifestField {
            files: super::MANIFESTS,
            path: "engines.node",
            parse: engines_node,
        },
        Signal::Probe("node"),
    ],
    writes: &[],
    caps: Capabilities {
        package_exec: Some(ExecCap {
            program: Some("npx"),
            argv: t!["--package", Package, "--", Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE,
        }),
        file_fallback: true,
        file_interpreters: &["node", "nodejs", "bun", "deno"],
        run_task: Some(RunTaskCap {
            argv: t!["--run", Task, Sep("--"), Args],
            sources: &[ProviderId::PackageJson],
        }),
        exec: Some(ExecCap {
            program: Some("npx"),
            argv: t![Name, Args],
            reach: Reach::Network,
            accepts: NameShape::BARE.union(NameShape::VERSIONED),
        }),
        run_file: Some(RunFileCap {
            unsupported: &[
                ("jsx", "Node has no JSX transform"),
                ("tsx", "Node has no TSX transform"),
            ],
            program: None,
            extensions: &["js", "mjs", "cjs", "ts", "mts", "cts"],
            argv: t![File, Args],
        }),
        test: Some(super::TEST),
        quiet: QuietSupport::unsupported("node --run has no host diagnostic switch"),
        ..Capabilities::NONE
    },
    tasks: None,
    version: Some(crate::version::read),
    hooks: Hooks {
        before_plan: Some(before_plan),
        ..Hooks::NONE
    },
};

fn before_plan(
    present: &runner_core::Present,
    op: &runner_core::Op<'_>,
    _: &runner_core::Policy,
    _: &mut Vec<runner_core::Warning>,
) -> Result<(), runner_core::Refusal> {
    if matches!(op, runner_core::Op::Run { .. })
        && let Some(version) = present.version.as_deref()
        && let Some(major) = version
            .trim_start_matches('v')
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
        && major < 22
    {
        return Err(runner_core::Refusal::Invalid(format!(
            "node task execution needs Node 22 or newer, but the node on PATH is {version}"
        )));
    }
    Ok(())
}
