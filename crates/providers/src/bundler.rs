//! Bundler.

use runner_core::{
    Capabilities, Discovery, Ecosystem, Frozen, Hooks, InstallCap, Kind, Provider, ProviderId,
    QuietSupport, ScriptSupport, Signal, TestCap, t,
};

/// Bundler.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Bundler,
    label: "bundler",
    aliases: &["bundle"],
    ecosystem: Ecosystem::Ruby,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("bundle"),
    signals: &[
        Signal::File("Gemfile"),
        Signal::Lockfile("Gemfile.lock"),
        Signal::Probe("bundle"),
    ],
    writes: &[],
    caps: Capabilities {
        install: Some(InstallCap {
            argv: t!["install"],
            frozen: Frozen::Unsupported,
            scripts: ScriptSupport::NONE,
            locked_only_with: &[],
        }),
        test: Some(TestCap {
            program: Some("rake"),
            argv: t!["test", Args],
            discovery: Discovery::Tool,
            file_flags: None,
        }),
        quiet: QuietSupport::NONE,
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
