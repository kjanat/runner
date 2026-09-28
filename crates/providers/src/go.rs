//! Go modules.

use runner_core::{
    Capabilities, CleanCap, Discovery, Ecosystem, ExecCap, Frozen, Hooks, InstallCap, Kind,
    NameShape, Provider, ProviderId, QuietSupport, Reach, RunFileCap, RunTaskCap, ScriptSupport,
    Signal, TestCap, t,
};

/// Go.
pub const PROVIDER: Provider = Provider {
    id: ProviderId::Go,
    label: "go",
    aliases: &["go.mod"],
    ecosystem: Ecosystem::Go,
    kind: Kind::PACKAGE_MANAGER.union(Kind::TASK_SOURCE),
    program: Some("go"),
    signals: &[
        Signal::File("go.mod"),
        Signal::Lockfile("go.sum"),
        Signal::Probe("go"),
    ],
    caps: Capabilities {
        variants: &[("vcs", VCS)],
        ..CAPS
    },
    tasks: Some(crate::extract::go_pm::tasks),
    version: None,
    hooks: Hooks {
        before_plan: None,
        after_observe: Some(crate::extract::go_pm::vcs_variant),
    },
};

/// Go inside a checkout it can stamp.
const VCS: Capabilities = Capabilities {
    run_task: Some(RunTaskCap {
        argv: t!["run", "-buildvcs=true", Task, Args],
        sources: &[ProviderId::Go],
    }),
    ..CAPS
};

const CAPS: Capabilities = Capabilities {
    writes: &["vendor"],
    task_priority: 7,
    file_fallback: true,
    install: Some(InstallCap {
        argv: t!["mod", "download"],
        frozen: Frozen::Unsupported,
        scripts: ScriptSupport::NONE,
        locked_only_with: &[],
        lockfiles: None,
    }),
    run_task: Some(RunTaskCap {
        argv: t!["run", Task, Args],
        sources: &[ProviderId::Go],
    }),
    exec: Some(ExecCap {
        program: None,
        argv: t!["run", Name, Args],
        reach: Reach::Network,
        accepts: NameShape::PATH_LIKE.union(NameShape::VERSIONED),
    }),
    run_file: Some(RunFileCap {
        unsupported: &[],
        program: None,
        extensions: &["go"],
        argv: t!["run", File, Args],
    }),
    test: Some(TestCap {
        program: None,
        argv: t!["test", "./...", Args],
        discovery: Discovery::Tool,
        file_flags: None,
    }),
    clean: Some(CleanCap {
        dir_suffixes: &[],
        framework_dirs: &[],
        dirs: &["vendor"],
    }),
    quiet: QuietSupport::unsupported("go run has no host-only quiet mode"),
    ..Capabilities::NONE
};
