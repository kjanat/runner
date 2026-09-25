//! A signal that was found.

use std::path::PathBuf;

use crate::provider::ProviderId;
use crate::scope::Scope;
use crate::signal::{Declared, SignalId};

/// How strongly a piece of evidence speaks. Ordered strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Weight {
    /// A manifest says so.
    Declared,
    /// A lockfile says so.
    Locked,
    /// A tool config exists.
    Configured,
    /// A directory the tool writes exists.
    Present,
    /// The executable is on `PATH`.
    Probed,
}

/// A signal that was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// The provider the signal belongs to, or `None` for a discovered file or binary.
    pub provider: Option<ProviderId>,
    /// Which of the provider's signals matched, when a provider owns the evidence.
    pub signal: Option<SignalId>,
    /// Where it was found.
    pub at: PathBuf,
    /// The workspace member it belongs to.
    pub scope: Scope,
    /// How strongly it speaks.
    pub weight: Weight,
    /// What the manifest declared, when the signal was a manifest field.
    pub declared: Option<Declared>,
}

impl Evidence {
    /// The order evidence is compared in, strongest first: weight, then the
    /// declaration's own rank.
    #[must_use]
    pub fn strength(&self) -> (Weight, u8) {
        (
            self.weight,
            self.declared.as_ref().map_or(0, Declared::rank),
        )
    }
}

/// A provider with enough evidence to count as part of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Present {
    /// The provider.
    pub provider: ProviderId,
    /// The scope it is present in.
    pub scope: Scope,
    /// The resolved version, when known.
    pub version: Option<String>,
    /// Directories holding executables the provider installed.
    pub bin_dirs: Vec<PathBuf>,
    /// The evidence that made it present, strongest first.
    pub because: Vec<Evidence>,
}
