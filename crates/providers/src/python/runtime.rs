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
    signals: &[
        Signal::ManifestField {
            files: &["pyproject.toml"],
            path: "project",
            parse: super::table,
        },
        Signal::ManifestField {
            files: &["pyproject.toml"],
            path: "build-system",
            parse: super::table,
        },
        Signal::File("setup.py"),
        Signal::File("requirements.txt"),
        Signal::Probe(if cfg!(windows) { "python" } else { "python3" }),
    ],
    caps: Capabilities {
        clean: Some(super::CLEAN),
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
