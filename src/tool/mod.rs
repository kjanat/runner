//! Per-tool modules: detection, task extraction, and command building.
//!
//! Each module corresponds to a single tool (package manager or task runner)
//! and exposes a consistent set of public functions:
//!
//! - `detect(dir)`, returns `true` if the tool's config/lockfile exists
//! - `extract_tasks(dir)`, parses config and returns task names or a parse error
//! - `run_cmd(task, args)`, builds a [`std::process::Command`] to run a task
//! - `install_cmd(frozen, scripts)`, builds a [`std::process::Command`] to install deps
//! - `exec_cmd(args)`, builds a [`std::process::Command`] for ad-hoc execution
//! - clean-dir constants, directories to remove on `runner clean`
//!
//! Not every module exposes every function; only what the tool supports.

/// bacon, Rust background checker (`bacon.toml`).
pub(crate) mod bacon;
/// Bun JavaScript runtime and package manager.
pub(crate) mod bun;
/// Bundler, the Ruby dependency manager (`Gemfile`).
pub(crate) mod bundler;
/// Cargo `[alias]` table, `.cargo/config.toml` aliases as runnable tasks.
pub(crate) mod cargo_aliases;
/// Cargo, the Rust package manager and build tool (`Cargo.toml`).
pub(crate) mod cargo_pm;
/// Composer, the PHP dependency manager (`composer.json`).
pub(crate) mod composer;
/// Deno JavaScript/TypeScript runtime (`deno.json` / `deno.jsonc`).
pub(crate) mod deno;
/// In-process execution of deno tasks via `deno_task_shell` (no deno binary).
pub(crate) mod deno_exec;
/// Shared filesystem helpers for tool modules.
pub(crate) mod files;
/// Git queries used by detection.
pub(crate) mod git;
/// Go modules (`go.mod`).
pub(crate) mod go_pm;
/// go-task, a task runner using `Taskfile.yml`.
pub(crate) mod go_task;
/// just, a command runner using `justfile`.
pub(crate) mod just;
/// GNU Make (`Makefile`).
pub(crate) mod make;
/// mise, a polyglot dev tool manager (`mise.toml`).
pub(crate) mod mise;
/// Shared Node.js helpers: `package.json` parsing, script extraction, PM detection.
pub(crate) mod node;
/// npm, the default Node.js package manager (`package-lock.json`).
pub(crate) mod npm;
/// Nx monorepo build system (`nx.json`).
pub(crate) mod nx;
/// Detect `package.json` scripts that wrap a known task runner.
pub(crate) mod passthrough;
/// Pipenv, a Python dependency manager (`Pipfile`).
pub(crate) mod pipenv;
/// pnpm, a fast Node.js package manager (`pnpm-lock.yaml`).
pub(crate) mod pnpm;
/// Poetry, a Python dependency manager (`poetry.lock`, `pyproject.toml`).
pub(crate) mod poetry;
/// Spawn helper with Windows-aware PATH/PATHEXT resolution.
pub(crate) mod program;
/// Shared Python tooling helpers.
pub(crate) mod python;
/// Cross-platform in-process shell runner (`deno_task_shell`), reusable
/// for any tool whose task bodies are shell command strings.
pub(crate) mod shell;
/// Turborepo monorepo build system (`turbo.json` / `turbo.jsonc`).
pub(crate) mod turbo;
/// uv, a fast Python package manager (`uv.lock`).
pub(crate) mod uv;
/// Volta toolchain manager, shim classification and `volta which` resolution.
pub(crate) mod volta;
/// Yarn, a Node.js package manager (`yarn.lock`).
pub(crate) mod yarn;

#[cfg(test)]
pub(crate) mod test_support;

/// How aggressively to silence the spawned host tool's **own** logging.
///
/// Ordered from loudest to quietest. Lowered from the resolved verbosity
/// intent at the run-dispatch boundary (`cmd::run::dispatch`) and carried in
/// [`HostVerbosity`] into each per-tool `run_cmd`. Most hosts saturate at
/// [`QuietLevel::Quiet`] (their `--silent` is the floor), so the finer rungs
/// only bite where a host exposes graduated loglevels (e.g. turbo's
/// `--output-logs`); above a host's floor they are an honest no-op.
///
/// The `Ord` derive gives the level comparisons the builders and the
/// runner-side gates rely on (`level >= QuietLevel::Quiet`), so the variant
/// order below is load-bearing: loudest first, quietest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum QuietLevel {
    /// No silencing; leave the host at its built-in verbosity.
    #[default]
    Off,
    /// Silence the host's own banner/log lines (`npm --silent`, `cargo -q`,
    /// `make -s`, …). `-q` / level 1.
    Quiet,
    /// Everything in [`Self::Quiet`] plus the host's lowest explicit loglevel
    /// where it distinguishes one (turbo `--output-logs=errors-only`). `-qq` /
    /// level 2. On the runner side this also folds in `--no-warnings`.
    VeryQuiet,
    /// Saturating floor: everything the host can suppress (turbo
    /// `--output-logs=none`). `-qqq` / level 3.
    Silent,
}

/// Whether to keep the host's **stdout** clean by diverting its diagnostics to
/// stderr. Orthogonal to [`QuietLevel`]: a caller can ask for a clean stdout
/// pipeline without silencing, or silence without diverting.
///
/// Only pnpm exposes the primitive (`--use-stderr`); every other host no-ops
/// this request silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Stream {
    /// Leave the host's stream routing untouched (stdout stays stdout).
    #[default]
    Inherit,
    /// Ask the host to write its own diagnostics to stderr, leaving stdout for
    /// the task's output (pnpm `--use-stderr`).
    Stderr,
}

/// The resolved, per-task verbosity intent handed to a host's `run_cmd`.
///
/// Combines the two orthogonal axes ([`QuietLevel`] and [`Stream`]). Each host
/// translates the parts it can express into its own flags and ignores the
/// rest; nothing here is an error when a host lacks a mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HostVerbosity {
    /// How much of the host's own logging to suppress.
    pub level: QuietLevel,
    /// Whether to divert the host's diagnostics to stderr.
    pub stream: Stream,
}

impl QuietLevel {
    /// Map a repeat count (`-q` = 1, `-qq` = 2, `-qqq`+ = 3) to a level.
    pub(crate) const fn from_count(count: u8) -> Self {
        match count {
            0 => Self::Off,
            1 => Self::Quiet,
            2 => Self::VeryQuiet,
            _ => Self::Silent,
        }
    }

    /// The count this level corresponds to, for round-tripping through the
    /// `RUNNER_QUIET` env marker set on spawned children.
    pub(crate) const fn as_count(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Quiet => 1,
            Self::VeryQuiet => 2,
            Self::Silent => 3,
        }
    }

    /// Parse a config/label spelling (`off` | `quiet` | `very-quiet` |
    /// `silent`). Returns `None` for anything else so callers can warn.
    pub(crate) fn from_label(raw: &str) -> Option<Self> {
        match raw.trim() {
            "off" => Some(Self::Off),
            "quiet" => Some(Self::Quiet),
            "very-quiet" => Some(Self::VeryQuiet),
            "silent" => Some(Self::Silent),
            _ => None,
        }
    }

    /// The canonical label, the inverse of [`Self::from_label`].
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Quiet => "quiet",
            Self::VeryQuiet => "very-quiet",
            Self::Silent => "silent",
        }
    }

    /// Every level, loudest first, for building "expected one of …" messages.
    pub(crate) const ALL: [Self; 4] = [Self::Off, Self::Quiet, Self::VeryQuiet, Self::Silent];
}

impl Stream {
    /// Parse a config/label spelling (`inherit` | `stderr`). `None` otherwise.
    pub(crate) fn from_label(raw: &str) -> Option<Self> {
        match raw.trim() {
            "inherit" => Some(Self::Inherit),
            "stderr" => Some(Self::Stderr),
            _ => None,
        }
    }

    /// The canonical label, the inverse of [`Self::from_label`].
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Stderr => "stderr",
        }
    }

    /// Both variants, for "expected one of …" messages.
    pub(crate) const ALL: [Self; 2] = [Self::Inherit, Self::Stderr];
}

impl HostVerbosity {
    /// `true` once the level reaches [`QuietLevel::Quiet`]: the host should get
    /// its silence flag. The single predicate every "does this host have just
    /// one silence flag" builder uses.
    pub(crate) fn silences(self) -> bool {
        self.level >= QuietLevel::Quiet
    }

    /// `true` when stdout should be kept clean by moving diagnostics to stderr.
    pub(crate) fn diverts_to_stderr(self) -> bool {
        self.stream == Stream::Stderr
    }
}

/// What an install command should do with lifecycle scripts.
///
/// Lowered from `crate::resolver::ScriptPolicy` at the install dispatch
/// boundary so the per-tool `install_cmd` builders stay decoupled from the
/// resolver. Each manager translates this into its own flag/env (or no-ops
/// where it cannot express the request, `cmd::install` warns about those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScriptDirective {
    /// Leave the package manager at its built-in default, add nothing.
    #[default]
    Default,
    /// Skip lifecycle scripts where the manager exposes a skip mechanism
    /// (npm/yarn-classic/pnpm/bun `--ignore-scripts`, composer `--no-scripts`,
    /// yarn-berry `YARN_ENABLE_SCRIPTS=false`).
    Deny,
    /// Force lifecycle scripts on where the manager exposes a mechanism
    /// (npm `--no-ignore-scripts`, yarn-berry `YARN_ENABLE_SCRIPTS=true`,
    /// deno `--allow-scripts`). Managers that already run scripts by default
    /// (composer, cargo, go, bundler, the Python backends, yarn-classic) need
    /// nothing; pnpm/bun gate dependency build scripts behind a manifest
    /// allowlist runner won't write, so the flag cannot express it.
    ForceOn,
}
