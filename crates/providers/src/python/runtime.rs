//! Python source-file execution.
use runner_core::{
    Capabilities, Ecosystem, Hooks, Kind, Provider, ProviderId, RunFileCap, Signal, t,
};
/// Python interpreter.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Python,
    label: "python",
    aliases: &["python3"],
    ecosystem: Ecosystem::Python,
    kind: Kind::RUNTIME,
    program: Some(if cfg!(windows) { "python" } else { "python3" }),
    signals: &[Signal::Probe(if cfg!(windows) {
        "python"
    } else {
        "python3"
    })],
    writes: &[],
    caps: Capabilities {
        file_fallback: true,
        run_file: Some(RunFileCap {
            unsupported: &[],
            program: None,
            argv: t![File, Args],
            extensions: &["py"],
        }),
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
