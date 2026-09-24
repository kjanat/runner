//! Composer.

use runner_core::{
    Capabilities, Ecosystem, Frozen, Hooks, InstallCap, Kind, Provider, ProviderId, QuietSupport,
    ScriptMechanism, ScriptSupport, Signal, t,
};

/// Composer.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Composer,
    label: "composer",
    aliases: &[],
    ecosystem: Ecosystem::Php,
    kind: Kind::PACKAGE_MANAGER,
    program: Some("composer"),
    signals: &[
        Signal::File("composer.json"),
        Signal::Lockfile("composer.lock"),
        Signal::Probe("composer"),
    ],
    writes: &["vendor"],
    caps: Capabilities {
        install: Some(InstallCap {
            argv: t!["install", Scripts],
            frozen: Frozen::Unsupported,
            scripts: ScriptSupport {
                deny: ScriptMechanism::Flag("--no-scripts"),
                allow: ScriptMechanism::Default,
            },
            locked_only_with: &[],
        }),
        quiet: QuietSupport::NONE,
        ..Capabilities::NONE
    },
    tasks: None,
    version: None,
    hooks: Hooks::NONE,
};
