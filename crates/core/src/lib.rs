//! Core types, pipeline and services for runner.

pub mod capability;
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
pub mod verbosity;
pub mod warning;

pub use capability::{
    BinDirs, BinsCap, Capabilities, CleanCap, Discovery, ExecCap, Frozen, HealthCap, InstallCap,
    NameShape, QuietSupport, RunFileCap, RunTaskCap, RuntimeCap, ScriptMechanism, ScriptSupport,
    TestCap, UsageCap, UsageSpec, WorkspaceCap,
};
pub use cascade::{CASCADE, Cap, Need, Rung};
pub use declare::{SETTINGS, Setting, SettingKind};
pub use env::{EnvLayers, EnvTable, LOADER_HOOKS, project_may_set};
pub use evidence::{Evidence, Present, Weight};
pub use execute::execute;
pub use health::Health;
pub use observe::observe;
pub use op::Op;
pub use plan::{
    Cascade, Clamp, ConfirmFn, DepFn, Dispatch, Plan, Refusal, Shebang, Trust, Unsafe, discover,
    dispatch, dispatch_from, ecosystem_of, file_plan, has_local_prefix, is_directly_executable,
    plan, plan_argv, plan_found, plan_with, read_shebang, resolve_path, scope_dir, select,
};
pub use policy::{Choice, Layer, PerEcosystem, Policy, ReachPolicy, ScriptPolicy, TrustPolicy};
pub use probe::{Prober, probe_in, probe_with};
pub use provider::{Ecosystem, Hooks, Kind, ProviderId};
pub use reach::Reach;
pub use registry::{AfterObserveFn, BeforePlanFn, Provider, Registry, TasksFn, VersionFn};
pub use resolve::{Project, resolve};
pub use scheme::{Check, ParseError, Scheme, check};
pub use scope::Scope;
pub use signal::{Declared, Signal, SignalId};
pub use task::{Task, TaskDetail};
pub use template::{Piece, Rendered, Request, ScriptRequest, Template};
pub use tree::Tree;
pub use verbosity::Verbosity;
pub use warning::Warning;
