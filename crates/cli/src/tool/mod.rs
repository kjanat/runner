//! Per-tool modules: detection, task extraction, and command building.
//!
//! Each module corresponds to a single tool (package manager or task runner)
//! and exposes a consistent set of public functions:
//!
//! - `detect(dir)`, returns `true` if the tool's config/lockfile exists
//! - `extract_tasks(dir)`, parses config and returns task names or a parse error
//! - `run_cmd(task, args)`, builds a [`std::process::Command`] to run a task
//! - `quiet_capabilities()`, declares safe host-only diagnostic reduction
//! - `exec_cmd(args)`, builds a [`std::process::Command`] for ad-hoc execution
//! - clean-dir constants, directories to remove on `runner clean`
//!
//! Not every module exposes every function; only what the tool supports. The
//! audited host contract is documented in `docs/host-quiet-support-matrix.md`.

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
/// Turborepo monorepo build system (`turbo.json` / `turbo.jsonc`).
pub(crate) mod turbo;
/// uv, a fast Python package manager (`uv.lock`).
pub(crate) mod uv;
/// Volta toolchain manager, shim classification and `volta which` resolution.
pub(crate) mod volta;
/// Workspace member discovery from root declarations.
pub(crate) mod workspace;
/// Yarn, a Node.js package manager (`yarn.lock`).
pub(crate) mod yarn;

#[cfg(test)]
pub(crate) mod test_support;

/// The repeated `-q` preset selected for runner output.
///
/// Ordering is used only for clamping and legacy config parsing. Runner output
/// categories are derived explicitly by [`OutputPolicy::from_quiet`], avoiding
/// accidental coupling between unrelated categories.
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
    /// Suppress recoverable runner error decoration and request stronger safe
    /// host diagnostics. Adapters clamp unsupported requests. `-qqq` / level 3.
    Silent,
    /// No runner-authored text. Task stdout/stderr remain inherited. `-qqqq` /
    /// level 4; larger counts clamp here.
    Mute,
}

/// Host-owned diagnostic reduction requested independently from runner output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) enum HostDiagnostics {
    /// Leave the host invocation unchanged.
    #[default]
    Normal,
    /// Apply the host's documented quiet mode when task output survives.
    Quiet,
    /// Request a stronger safe reduction; adapters clamp when unsupported.
    Reduced,
}

/// Whether one task stream is inherited or explicitly discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TaskStream {
    #[default]
    Inherit,
    Discard,
}

/// One runner-authored output category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunnerOutput {
    Progress,
    Warnings,
    Errors,
    Groups,
    TaskTiming,
    Summary,
    FatalErrors,
}

impl RunnerOutput {
    pub(crate) const ALL: [Self; 7] = [
        Self::Progress,
        Self::Warnings,
        Self::Errors,
        Self::Groups,
        Self::TaskTiming,
        Self::Summary,
        Self::FatalErrors,
    ];

    /// The `[runner]` key and report field naming this category.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::Warnings => "warnings",
            Self::Errors => "errors",
            Self::Groups => "groups",
            Self::TaskTiming => "task_timing",
            Self::Summary => "summary",
            Self::FatalErrors => "fatal_errors",
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Progress => 1,
            Self::Warnings => 1 << 1,
            Self::Errors => 1 << 2,
            Self::Groups => 1 << 3,
            Self::TaskTiming => 1 << 4,
            Self::Summary => 1 << 5,
            Self::FatalErrors => 1 << 6,
        }
    }
}

/// The runner-authored output categories an invocation shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunnerOutputPolicy(u8);

impl RunnerOutputPolicy {
    pub(crate) const ALL: Self = Self::of(&RunnerOutput::ALL);

    /// Exactly `outputs` shown.
    pub(crate) const fn of(outputs: &[RunnerOutput]) -> Self {
        let mut bits = 0;
        let mut index = 0;
        while index < outputs.len() {
            bits |= outputs[index].bit();
            index += 1;
        }
        Self(bits)
    }

    pub(crate) const fn shows(self, output: RunnerOutput) -> bool {
        self.0 & output.bit() != 0
    }

    #[must_use]
    pub(crate) const fn with(self, output: RunnerOutput, shown: bool) -> Self {
        if shown {
            Self(self.0 | output.bit())
        } else {
            Self(self.0 & !output.bit())
        }
    }

    #[must_use]
    pub(crate) const fn and(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }
}

impl std::fmt::Display for RunnerOutputPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, output) in RunnerOutput::ALL.into_iter().enumerate() {
            let separator = if index == 0 { "" } else { " " };
            write!(f, "{separator}{}={}", output.label(), self.shows(output))?;
        }
        Ok(())
    }
}

impl serde::Serialize for RunnerOutputPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(RunnerOutput::ALL.len()))?;
        for output in RunnerOutput::ALL {
            map.serialize_entry(output.label(), &self.shows(output))?;
        }
        map.end()
    }
}

impl schemars::JsonSchema for RunnerOutputPolicy {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RunnerOutputPolicy".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let labels = RunnerOutput::ALL.map(RunnerOutput::label);
        let properties: serde_json::Map<String, serde_json::Value> = labels
            .iter()
            .map(|label| {
                (
                    (*label).to_owned(),
                    serde_json::json!({ "type": "boolean" }),
                )
            })
            .collect();
        schemars::json_schema!({
            "type": "object",
            "properties": properties,
            "required": labels,
        })
    }
}

/// Effective output policy. Quiet levels are presets over these axes; task
/// streams never change from a quiet preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputPolicy {
    pub runner: RunnerOutputPolicy,
    pub host_diagnostics: HostDiagnostics,
}

impl Default for OutputPolicy {
    fn default() -> Self {
        Self::from_quiet(QuietLevel::Off)
    }
}

impl OutputPolicy {
    pub(crate) const fn from_quiet(level: QuietLevel) -> Self {
        match level {
            QuietLevel::Off => Self {
                runner: RunnerOutputPolicy::ALL,
                host_diagnostics: HostDiagnostics::Normal,
            },
            QuietLevel::Quiet => Self {
                runner: RunnerOutputPolicy::of(&[
                    RunnerOutput::Warnings,
                    RunnerOutput::Errors,
                    RunnerOutput::FatalErrors,
                ]),
                host_diagnostics: HostDiagnostics::Normal,
            },
            QuietLevel::VeryQuiet => Self {
                runner: RunnerOutputPolicy::of(&[RunnerOutput::Errors, RunnerOutput::FatalErrors]),
                host_diagnostics: HostDiagnostics::Quiet,
            },
            QuietLevel::Silent => Self {
                runner: RunnerOutputPolicy::of(&[RunnerOutput::FatalErrors]),
                host_diagnostics: HostDiagnostics::Reduced,
            },
            QuietLevel::Mute => Self {
                runner: RunnerOutputPolicy::of(&[]),
                host_diagnostics: HostDiagnostics::Reduced,
            },
        }
    }
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
    pub diagnostics: HostDiagnostics,
    /// Whether to divert the host's diagnostics to stderr.
    pub stream: Stream,
}

impl QuietLevel {
    /// Map a repeat count to the named ladder, clamping at [`Self::Mute`].
    pub(crate) const fn from_count(count: u8) -> Self {
        match count {
            0 => Self::Off,
            1 => Self::Quiet,
            2 => Self::VeryQuiet,
            3 => Self::Silent,
            _ => Self::Mute,
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
            Self::Mute => 4,
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
            "mute" => Some(Self::Mute),
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
            Self::Mute => "mute",
        }
    }

    /// Every level, loudest first, for building "expected one of …" messages.
    pub(crate) const ALL: [Self; 5] = [
        Self::Off,
        Self::Quiet,
        Self::VeryQuiet,
        Self::Silent,
        Self::Mute,
    ];
}

impl HostDiagnostics {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Quiet => "quiet",
            Self::Reduced => "reduced",
        }
    }

    pub(crate) fn from_label(raw: &str) -> Option<Self> {
        match raw.trim() {
            "normal" | "off" => Some(Self::Normal),
            "quiet" => Some(Self::Quiet),
            "reduced" | "very-quiet" | "silent" | "mute" => Some(Self::Reduced),
            _ => None,
        }
    }

    pub(crate) const fn from_legacy_quiet(level: QuietLevel) -> Self {
        match level {
            QuietLevel::Off => Self::Normal,
            QuietLevel::Quiet => Self::Quiet,
            QuietLevel::VeryQuiet | QuietLevel::Silent | QuietLevel::Mute => Self::Reduced,
        }
    }
}

impl TaskStream {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Discard => "discard",
        }
    }

    pub(crate) fn from_label(raw: &str) -> Option<Self> {
        match raw.trim() {
            "inherit" => Some(Self::Inherit),
            "discard" => Some(Self::Discard),
            _ => None,
        }
    }
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
    /// Whether the applied host plan includes its safe quiet flag.
    #[cfg(test)]
    pub(crate) fn silences(self) -> bool {
        self.diagnostics >= HostDiagnostics::Quiet
    }

    /// `true` when stdout should be kept clean by moving diagnostics to stderr.
    #[cfg(test)]
    pub(crate) fn diverts_to_stderr(self) -> bool {
        self.stream == Stream::Stderr
    }
}

#[cfg(test)]
mod quiet_policy_tests {
    use super::{HostDiagnostics, OutputPolicy, QuietLevel};

    #[test]
    fn quiet_counts_have_four_distinct_levels_and_clamp() {
        assert_eq!(QuietLevel::from_count(0), QuietLevel::Off);
        assert_eq!(QuietLevel::from_count(1), QuietLevel::Quiet);
        assert_eq!(QuietLevel::from_count(2), QuietLevel::VeryQuiet);
        assert_eq!(QuietLevel::from_count(3), QuietLevel::Silent);
        assert_eq!(QuietLevel::from_count(4), QuietLevel::Mute);
        assert_eq!(QuietLevel::from_count(u8::MAX), QuietLevel::Mute);
    }

    #[test]
    fn host_diagnostics_begin_at_second_quiet_level() {
        assert_eq!(
            OutputPolicy::from_quiet(QuietLevel::Quiet).host_diagnostics,
            HostDiagnostics::Normal
        );
        assert_eq!(
            OutputPolicy::from_quiet(QuietLevel::VeryQuiet).host_diagnostics,
            HostDiagnostics::Quiet
        );
    }
}
