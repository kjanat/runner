//! Core types, pipeline and services for runner.

mod capability;
pub mod cascade;
pub mod declare;
pub mod env;
pub mod evidence;
pub mod execute;
pub mod health;
pub mod observe;
pub mod op;
pub mod plan;
pub mod policy;
pub mod probe;
pub mod provider;
pub mod reach;
pub mod registry;
pub mod resolve;
pub mod scheme;
pub mod scope;
pub mod signal;
pub mod task;
pub mod template;
pub mod tree;
pub mod usage;
pub mod verbosity;
pub mod warning;
pub mod workspace;

pub use capability::{
    BinDirs, BinRuns, BinsCap, Capabilities, CleanCap, Discovery, ExecCap, FileFlagsFn, Frozen,
    HealthCap, InstallCap, Installed, InstalledBin, Lockfiles, NameShape, PackagesCap,
    QuietSupport, RunFileCap, RunTaskCap, RuntimeCap, ScriptMechanism, ScriptSupport, Shim,
    ShimsCap, TaskTable, TestCap, UsageCap, WorkspaceCap,
};
pub use cascade::{CASCADE, Cap, Need, Rung};
pub use declare::{SETTINGS, Setting, SettingKind};
pub use env::{EnvLayers, EnvTable, LOADER_HOOKS, project_may_set};
pub use evidence::{Evidence, Present, Weight};
pub use execute::execute;
pub use health::Health;
pub use observe::{observe, read_manifest};
pub use op::Op;
pub use plan::{
    Cascade, Clamp, ConfirmFn, DepFn, Dispatch, Plan, Refusal, Shebang, TaskRank, Trust, Unsafe,
    decided_by, dependency_plan, discover, dispatch, dispatch_from, ecosystem_of, file_plan,
    has_local_prefix, is_directly_executable, plan, plan_argv, plan_bin, plan_found, plan_with,
    ranked_tasks, read_shebang, resolve_path, scope_dir, select,
};
pub use policy::{
    Choice, Layer, OnMismatch, PerEcosystem, Policy, ReachPolicy, ScriptPolicy, TrustPolicy,
};
pub use probe::{Prober, probe_in, probe_in_dirs, probe_with};
pub use provider::{Ecosystem, Hooks, Kind, ProviderId};
pub use reach::Reach;
pub use registry::{AfterObserveFn, BeforePlanFn, Provider, Registry, TasksFn, VersionFn};
pub use resolve::{Disagreement, Project, Unread, prefer_tracked_lockfiles, resolve};
pub use scheme::{Check, ParseError, Scheme, check};
pub use scope::Scope;
pub use signal::{Declared, Field, OnFail, Signal, SignalId};
pub use task::{Extracted, Task, TaskDetail};
pub use template::{Piece, Rendered, Request, ScriptRequest, Template};
pub use tree::Tree;
pub use usage::{UsageArg, UsageFlag, UsageSpec, usage};
pub use verbosity::Verbosity;
pub use warning::Warning;
pub use workspace::{Declaration, Member, Workspace};

mod script;

pub mod clean;
