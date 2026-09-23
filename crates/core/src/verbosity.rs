//! The host diagnostic level a task asks for.

/// How much of the host's own output to suppress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Verbosity {
    /// The host's built-in verbosity.
    #[default]
    Normal,
    /// The host's quiet mode.
    Quiet,
    /// The host's lowest log level.
    VeryQuiet,
    /// Everything the host can safely drop.
    Silent,
}

impl Verbosity {
    /// Every level, loudest first.
    pub const ALL: [Self; 4] = [Self::Normal, Self::Quiet, Self::VeryQuiet, Self::Silent];

    /// Index into a provider's quiet ladder.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Normal => 0,
            Self::Quiet => 1,
            Self::VeryQuiet => 2,
            Self::Silent => 3,
        }
    }

    /// The label config and reports use.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Quiet => "quiet",
            Self::VeryQuiet => "very-quiet",
            Self::Silent => "silent",
        }
    }
}
