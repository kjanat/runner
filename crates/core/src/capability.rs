//! What a present provider can do.

use std::path::{Path, PathBuf};

use crate::evidence::Present;
use crate::health::Health;
use crate::provider::ProviderId;
use crate::reach::Reach;
use crate::task::Task;
use crate::template::Template;
use crate::warning::Warning;

/// Where a task source's file keeps its tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTable {
    /// The file has no per-task location.
    None,
    /// Tasks sit under this key path.
    Key(&'static str),
    /// Each task is a top-level target named after it.
    Name,
}

/// Everything a provider can do, each with the parameters policy can turn on.
#[derive(Clone, Copy)]
pub struct Capabilities {
    /// Use this runtime for supported files when no project runtime takes them.
    pub file_fallback: bool,
    /// Shebang interpreters this runtime can replace when explicitly chosen.
    pub file_interpreters: &'static [&'static str],
    /// Default source priority when policy has not ranked a source.
    pub task_priority: u8,
    /// Where the provider's task file keeps its tasks.
    pub task_table: TaskTable,
    /// Order among package managers probed on `PATH` for a task source none runs.
    pub probe_priority: u8,
    /// Capability tables selected by variant evidence from observation.
    pub variants: &'static [(&'static str, Self)],
    /// The variant the installed executable's version implies, consulted
    /// when no project evidence names one.
    pub variant_of_version: Option<fn(&str) -> Option<&'static str>>,
    /// Install dependencies.
    pub install: Option<InstallCap>,
    /// Invoke the task runner without naming a task.
    pub run_default: Option<Template>,
    /// Run a declared task.
    pub run_task: Option<RunTaskCap>,
    /// Execute a binary from an explicitly named package.
    pub package_exec: Option<ExecCap>,
    /// Execute a name.
    pub exec: Option<ExecCap>,
    /// Run a source file.
    pub run_file: Option<RunFileCap>,
    /// Run the test runner.
    pub test: Option<TestCap>,
    /// Where installed executables live.
    pub bins: Option<BinsCap>,
    /// Install directories this provider materialises, relative to the scope.
    pub writes: &'static [&'static str],
    /// Installed packages and the binaries they declare.
    pub packages: Option<PackagesCap>,
    /// Shims this tool manager puts on `PATH`.
    pub shims: Option<ShimsCap>,
    /// What `clean` removes.
    pub clean: Option<CleanCap>,
    /// Workspace member discovery.
    pub workspaces: Option<WorkspaceCap>,
    /// A read-only self check.
    pub health: &'static [HealthCap],
    /// Per-task argument specs.
    pub usage: Option<UsageCap>,
    /// Templates that differ when this provider is the chosen runtime.
    pub as_runtime: Option<RuntimeCap>,
    /// The runtime this package manager runs tasks on.
    pub runs_on: Option<ProviderId>,
    /// Operations a tool manager accepts on install.
    pub operations: &'static [&'static str],
    /// The quiet ladder.
    pub quiet: QuietSupport,
}

impl Capabilities {
    /// A provider that can do nothing yet.
    pub const NONE: Self = Self {
        file_fallback: false,
        file_interpreters: &[],
        task_priority: 2,
        task_table: TaskTable::None,
        probe_priority: 0,
        variants: &[],
        variant_of_version: None,
        install: None,
        run_default: None,
        run_task: None,
        package_exec: None,
        exec: None,
        run_file: None,
        test: None,
        bins: None,
        writes: &[],
        packages: None,
        shims: None,
        clean: None,
        workspaces: None,
        health: &[],
        usage: None,
        as_runtime: None,
        runs_on: None,
        operations: &[],
        quiet: QuietSupport::NONE,
    };
}

/// How a provider does a frozen install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frozen {
    /// A flag at the `Frozen` piece.
    Flag(&'static str),
    /// A different argv altogether.
    Argv(Template),
    /// An environment variable.
    Env(&'static str, &'static str),
    /// The provider has no frozen mode.
    Unsupported,
}

/// How a provider honours one script policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptMechanism {
    /// A flag at the `Scripts` piece.
    Flag(&'static str),
    /// An environment variable.
    Env(&'static str, &'static str),
    /// A flag and an environment variable, for a tool whose variant is unknown.
    FlagAndEnv(&'static str, &'static str, &'static str),
    /// The provider already behaves this way.
    Default,
    /// The provider cannot express it.
    Unsupported,
    /// The provider cannot express it and the plan should say why.
    Warn(&'static str),
}

/// How a provider honours deny and allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptSupport {
    /// Skip lifecycle scripts.
    pub deny: ScriptMechanism,
    /// Run lifecycle scripts.
    pub allow: ScriptMechanism,
}

impl ScriptSupport {
    /// The provider offers no script switch and always runs its build scripts.
    pub const NONE: Self = Self {
        deny: ScriptMechanism::Unsupported,
        allow: ScriptMechanism::Default,
    };
}

/// Install dependencies.
#[derive(Clone, Copy)]
pub struct InstallCap {
    /// The argv.
    pub argv: Template,
    /// The frozen mechanism.
    pub frozen: Frozen,
    /// The script mechanisms.
    pub scripts: ScriptSupport,
    /// Config/lockfile pairs; any existing pair enables frozen mode. Empty is unconditional.
    pub locked_only_with: &'static [(&'static str, &'static str)],
    /// The lockfiles a scope directory may hold beyond the provider's lockfile signals.
    pub lockfiles: Option<Lockfiles>,
}

/// The lockfiles a scope directory may hold beyond the provider's lockfile signals.
#[derive(Clone, Copy)]
pub enum Lockfiles {
    /// Fixed names relative to the scope.
    Named(&'static [&'static str]),
    /// Read from the project's configuration.
    Ask(fn(&Path) -> std::io::Result<Vec<PathBuf>>),
}

impl Lockfiles {
    /// The paths under `dir`.
    ///
    /// # Errors
    /// Returns the configuration read failure.
    pub fn paths(self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        match self {
            Self::Named(names) => Ok(names.iter().map(|name| dir.join(name)).collect()),
            Self::Ask(ask) => ask(dir),
        }
    }
}

/// Run a declared task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunTaskCap {
    /// The argv.
    pub argv: Template,
    /// Task sources this provider dispatches.
    pub sources: &'static [ProviderId],
}

bitflags::bitflags! {
    /// The name shapes an exec primitive takes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NameShape: u8 {
        /// A bare package or binary name.
        const BARE = 1;
        /// A name with a path separator.
        const PATH_LIKE = 2;
        /// A name with a version suffix.
        const VERSIONED = 4;
        /// A registry specifier such as `jsr:@std/http` or `npm:cowsay`.
        const REGISTRY = 8;
    }
}

/// The argv a runtime uses when policy names it, where it differs from the
/// package-manager form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCap {
    /// Execute a binary from a selected package on this runtime.
    pub package_exec: Option<Template>,
    /// Run a declared task on this runtime.
    pub run_task: Option<Template>,
    /// Execute a name on this runtime.
    pub exec: Option<Template>,
}

/// Execute a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecCap {
    /// The program, when it differs from the provider's.
    pub program: Option<&'static str>,
    /// The argv.
    pub argv: Template,
    /// Whether it can fetch.
    pub reach: Reach,
    /// The name shapes it takes.
    pub accepts: NameShape,
}

/// Run a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunFileCap {
    /// Recognized extensions this runtime refuses, with a user-facing reason.
    pub unsupported: &'static [(&'static str, &'static str)],
    /// The program, when it differs from the provider's.
    pub program: Option<&'static str>,
    /// File extensions it takes, without the dot.
    pub extensions: &'static [&'static str],
    /// The argv.
    pub argv: Template,
}

impl RunFileCap {
    /// Whether this capability accepts the file's extension.
    #[must_use]
    pub fn supports(&self, file: &Path) -> bool {
        self.refusal(file).is_none()
            && file
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    self.extensions
                        .iter()
                        .any(|known| ext.eq_ignore_ascii_case(known))
                })
    }

    /// The declared limitation for the file's extension.
    #[must_use]
    pub fn refusal(&self, file: &Path) -> Option<&'static str> {
        let extension = file.extension()?.to_str()?;
        self.unsupported
            .iter()
            .find(|(ext, _)| extension.eq_ignore_ascii_case(ext))
            .map(|(_, reason)| *reason)
    }
}

/// How a test runner finds its tests.
#[derive(Clone, Copy)]
pub enum Discovery {
    /// The runner finds its own files.
    Tool,
    /// The core passes files matching these globs.
    Files(&'static [&'static str]),
    /// The runner is itself a finding, looked for in each directory in turn.
    Detect(fn(&[&Path]) -> std::io::Result<Option<Template>>),
}

/// Run the test runner.
#[derive(Clone, Copy)]
pub struct TestCap {
    /// The program, when it differs from the provider's.
    pub program: Option<&'static str>,
    /// The argv.
    pub argv: Template,
    /// How tests are found.
    pub discovery: Discovery,
    /// The flags the discovered files call for, rendered at [`crate::Piece::FileFlags`].
    pub file_flags: Option<FileFlagsFn>,
}

/// The flags a test runner needs for the files it is handed.
pub type FileFlagsFn = fn(&[PathBuf]) -> &'static [&'static str];

/// Where installed executables live.
#[derive(Clone, Copy)]
pub enum BinDirs {
    /// Fixed directories relative to the scope.
    Static(&'static [&'static str]),
    /// The tool reports them.
    Ask(fn(&Path) -> std::io::Result<Vec<PathBuf>>),
}

/// Where installed executables live.
#[derive(Clone, Copy)]
pub struct BinsCap {
    /// The directories.
    pub dirs: BinDirs,
}

/// Installed packages and the binaries they declare.
#[derive(Clone, Copy)]
pub struct PackagesCap {
    /// The package `name` as installed for the directory, `None` when it is not.
    pub installed: fn(&crate::Tree, &Path, &str) -> Result<Option<Installed>, Warning>,
}

/// A package as installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Where it is installed.
    pub at: PathBuf,
    /// The binaries it declares, in declaration order.
    pub bins: Vec<InstalledBin>,
    /// The binary named after the package, when it declares one.
    pub default_bin: Option<String>,
}

/// One binary an installed package declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBin {
    /// The binary name.
    pub name: String,
    /// How it runs.
    pub runs: BinRuns,
}

/// How an installed binary runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinRuns {
    /// Execute this file.
    File(PathBuf),
    /// Hand the name to the provider's exec primitive.
    Exec,
}

/// Shims a tool manager puts on `PATH` in front of the tools it provisions.
#[derive(Clone, Copy)]
pub struct ShimsCap {
    /// The directories whose executables are shims, from the host environment.
    pub dirs: fn() -> Vec<PathBuf>,
    /// What the shim for a tool runs from a project directory.
    pub resolve: fn(&str, &Path) -> Shim,
}

/// What a shim runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shim {
    /// The tool is provisioned; this is the real binary.
    Resolved(PathBuf),
    /// The manager has no version of the tool.
    NotProvisioned,
    /// The manager did not answer.
    Unknown,
}

/// What `clean` removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanCap {
    /// Suffixes of generated directory names in the scope root.
    pub dir_suffixes: &'static [&'static str],
    /// Framework directories removed on explicit opt-in.
    pub framework_dirs: &'static [&'static str],
    /// Directories relative to the scope.
    pub dirs: &'static [&'static str],
}

/// Workspace member discovery.
#[derive(Clone, Copy)]
pub struct WorkspaceCap {
    /// The workspace declarations in a directory.
    pub declarations: fn(&Path) -> Result<Vec<crate::workspace::Declaration>, Warning>,
}

/// A read-only self check.
#[derive(Clone, Copy)]
pub struct HealthCap {
    /// The argv.
    pub argv: Template,
    /// Reads the output.
    pub parse: fn(&[u8]) -> Health,
}

/// Per-task argument specs.
#[derive(Clone, Copy)]
pub struct UsageCap {
    /// The spec for a task, when it has one.
    pub spec: fn(&crate::Tree, &Present, &Task) -> Result<Option<crate::UsageSpec>, Warning>,
}

/// The quiet ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietSupport {
    /// The flags for each [`crate::verbosity::Verbosity`], by index.
    pub levels: [Option<Template>; 4],
    /// Why the ladder stops where it does.
    pub limitation: &'static str,
}

impl QuietSupport {
    /// A host with no safe quiet switch.
    pub const NONE: Self = Self::unsupported("no host-only quiet mode");

    /// A host whose only quiet switch would eat task output.
    #[must_use]
    pub const fn unsupported(limitation: &'static str) -> Self {
        Self {
            levels: [None; 4],
            limitation,
        }
    }

    /// One flag at the quiet level, which [`Self::at`] also answers stronger
    /// requests with.
    #[must_use]
    pub const fn flag(template: Template) -> Self {
        Self {
            levels: [None, Some(template), None, None],
            limitation: "no stronger task-output-preserving reduction",
        }
    }

    /// The flags for `level`, clamped to the strongest the host supports.
    #[must_use]
    pub fn at(&self, level: usize) -> Option<Template> {
        self.levels[..=level.min(3)]
            .iter()
            .rev()
            .find_map(|template| *template)
    }

    /// The strongest level the host supports.
    #[must_use]
    pub fn strongest(&self) -> usize {
        self.levels.iter().rposition(Option::is_some).unwrap_or(0)
    }
}
