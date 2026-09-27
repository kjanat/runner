//! Filesystem, git and spawn helpers, and the output policy shared by every
//! command.

/// Shared filesystem helpers.
pub(crate) mod files;
/// Git queries used by detection.
pub(crate) mod git;
/// Spawn helper with Windows-aware PATH/PATHEXT resolution.
pub(crate) mod program;

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
    Timing,
    Summary,
    FatalErrors,
}

impl RunnerOutput {
    pub(crate) const ALL: [Self; 7] = [
        Self::Progress,
        Self::Warnings,
        Self::Errors,
        Self::Groups,
        Self::Timing,
        Self::Summary,
        Self::FatalErrors,
    ];

    /// The `[output]` key and report field naming this category.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::Warnings => "warnings",
            Self::Errors => "errors",
            Self::Groups => "groups",
            Self::Timing => "timing",
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
            Self::Timing => 1 << 4,
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

/// The resolved, per-task host verbosity handed to a host's command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HostVerbosity {
    /// How much of the host's own logging to suppress.
    pub diagnostics: HostDiagnostics,
}

/// The output settings one layer (command line, environment, a task's config,
/// the project's config) sets, each unset setting left to the layers below.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OutputChoice {
    set: u8,
    shown: u8,
    tool: Option<HostDiagnostics>,
    stdout: Option<bool>,
    stderr: Option<bool>,
}

/// The output an invocation or a task ends up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub runner: RunnerOutputPolicy,
    pub tool: HostDiagnostics,
    pub stdout: TaskStream,
    pub stderr: TaskStream,
}

impl OutputChoice {
    /// The settings `-q` repeated `level` times turns off.
    pub(crate) const fn preset(level: QuietLevel) -> Self {
        let hidden: &[RunnerOutput] = match level {
            QuietLevel::Off => &[],
            QuietLevel::Quiet => &[
                RunnerOutput::Progress,
                RunnerOutput::Groups,
                RunnerOutput::Timing,
                RunnerOutput::Summary,
            ],
            QuietLevel::VeryQuiet => &[
                RunnerOutput::Progress,
                RunnerOutput::Groups,
                RunnerOutput::Timing,
                RunnerOutput::Summary,
                RunnerOutput::Warnings,
            ],
            QuietLevel::Silent => &[
                RunnerOutput::Progress,
                RunnerOutput::Groups,
                RunnerOutput::Timing,
                RunnerOutput::Summary,
                RunnerOutput::Warnings,
                RunnerOutput::Errors,
            ],
            QuietLevel::Mute => &RunnerOutput::ALL,
        };
        let set = RunnerOutputPolicy::of(hidden).0;
        Self {
            set,
            shown: 0,
            tool: match level {
                QuietLevel::Off | QuietLevel::Quiet => None,
                QuietLevel::VeryQuiet => Some(HostDiagnostics::Quiet),
                QuietLevel::Silent | QuietLevel::Mute => Some(HostDiagnostics::Reduced),
            },
            stdout: None,
            stderr: None,
        }
    }

    /// This layer with `output` set to `shown`.
    #[must_use]
    pub(crate) const fn with(mut self, output: RunnerOutput, shown: bool) -> Self {
        self.set |= output.bit();
        if shown {
            self.shown |= output.bit();
        } else {
            self.shown &= !output.bit();
        }
        self
    }

    /// This layer with `output` set when `shown` is.
    #[must_use]
    pub(crate) const fn with_some(self, output: RunnerOutput, shown: Option<bool>) -> Self {
        match shown {
            Some(shown) => self.with(output, shown),
            None => self,
        }
    }

    /// This layer with the tool's quiet flag requested or not, when `quiet` is set.
    #[must_use]
    pub(crate) const fn with_tool_quiet(mut self, quiet: Option<bool>) -> Self {
        if let Some(quiet) = quiet {
            self.tool = Some(if quiet {
                HostDiagnostics::Quiet
            } else {
                HostDiagnostics::Normal
            });
        }
        self
    }

    /// This layer with the task's streams shown or discarded, where set.
    #[must_use]
    pub(crate) const fn with_streams(mut self, stdout: Option<bool>, stderr: Option<bool>) -> Self {
        if stdout.is_some() {
            self.stdout = stdout;
        }
        if stderr.is_some() {
            self.stderr = stderr;
        }
        self
    }

    /// `self` where it sets a value, `lower` elsewhere.
    #[must_use]
    pub(crate) const fn over(self, lower: Self) -> Self {
        Self {
            set: self.set | lower.set,
            shown: (self.shown & self.set) | (lower.shown & lower.set & !self.set),
            tool: if self.tool.is_some() {
                self.tool
            } else {
                lower.tool
            },
            stdout: if self.stdout.is_some() {
                self.stdout
            } else {
                lower.stdout
            },
            stderr: if self.stderr.is_some() {
                self.stderr
            } else {
                lower.stderr
            },
        }
    }

    /// The output with every unset setting at its default: everything shown,
    /// the tool at its own verbosity.
    pub(crate) const fn resolve(self) -> Resolved {
        const fn stream(shown: Option<bool>) -> TaskStream {
            match shown {
                Some(false) => TaskStream::Discard,
                _ => TaskStream::Inherit,
            }
        }
        Resolved {
            runner: RunnerOutputPolicy(
                (self.shown & self.set) | (RunnerOutputPolicy::ALL.0 & !self.set),
            ),
            tool: match self.tool {
                Some(tool) => tool,
                None => HostDiagnostics::Normal,
            },
            stdout: stream(self.stdout),
            stderr: stream(self.stderr),
        }
    }
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

    /// The canonical label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Quiet => "quiet",
            Self::VeryQuiet => "very-quiet",
            Self::Silent => "silent",
            Self::Mute => "mute",
        }
    }
}

impl HostDiagnostics {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Quiet => "quiet",
            Self::Reduced => "reduced",
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
}

#[cfg(test)]
mod quiet_policy_tests {
    use super::{HostDiagnostics, OutputChoice, QuietLevel, RunnerOutput, TaskStream};

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
        let tool = |level| OutputChoice::preset(level).resolve().tool;
        assert_eq!(tool(QuietLevel::Quiet), HostDiagnostics::Normal);
        assert_eq!(tool(QuietLevel::VeryQuiet), HostDiagnostics::Quiet);
        assert_eq!(tool(QuietLevel::Mute), HostDiagnostics::Reduced);
    }

    #[test]
    fn a_higher_layer_wins_only_where_it_sets_a_value() {
        let project = OutputChoice::default()
            .with(RunnerOutput::Timing, false)
            .with(RunnerOutput::Progress, false)
            .with_streams(Some(true), Some(false));
        let task = OutputChoice::default()
            .with(RunnerOutput::Timing, true)
            .with_streams(None, Some(true));
        let resolved = task.over(project).resolve();
        assert!(resolved.runner.shows(RunnerOutput::Timing));
        assert!(!resolved.runner.shows(RunnerOutput::Progress));
        assert!(resolved.runner.shows(RunnerOutput::Summary));
        assert_eq!(resolved.stdout, TaskStream::Inherit);
        assert_eq!(resolved.stderr, TaskStream::Inherit);
        let quiet = OutputChoice::preset(QuietLevel::Quiet).over(task.over(project));
        assert!(!quiet.resolve().runner.shows(RunnerOutput::Timing));
        let explicit =
            OutputChoice::preset(QuietLevel::VeryQuiet).with(RunnerOutput::Warnings, true);
        assert!(explicit.resolve().runner.shows(RunnerOutput::Warnings));
    }
}
