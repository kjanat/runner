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
            file: "package.json",
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
    version: None,
    hooks: Hooks::NONE,
};
