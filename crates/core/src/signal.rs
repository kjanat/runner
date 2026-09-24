//! What a provider tells the core to look for.

use std::io;
use std::path::Path;

use crate::evidence::Evidence;
use crate::provider::ProviderId;

/// Something to look for in a scope directory.
#[derive(Clone, Copy)]
pub enum Signal {
    /// A file in the scope directory.
    File(&'static str),
    /// A file in the scope directory or an ancestor.
    FileUpwards(&'static str),
    /// A file that also pins the provider.
    Lockfile(&'static str),
    /// A manifest field that names or constrains the provider.
    ManifestField {
        /// The manifest file.
        file: &'static str,
        /// Dotted path of the field.
        path: &'static str,
        /// Reads the field into a declaration, `None` when it names another provider.
        parse: fn(&serde_json::Value) -> Option<Declared>,
    },
    /// A variable set in runner's own environment.
    EnvVar(&'static str),
    /// An executable on `PATH`, checked last.
    Probe(&'static str),
    /// A read-only subprocess that reports its own evidence.
    Ask(fn(&Path) -> io::Result<Vec<Evidence>>),
}

impl Signal {
    /// The file or variable name the signal looks for, when it has one.
    #[must_use]
    pub const fn name(&self) -> Option<&'static str> {
        match self {
            Self::File(name)
            | Self::FileUpwards(name)
            | Self::Lockfile(name)
            | Self::EnvVar(name)
            | Self::Probe(name) => Some(name),
            Self::ManifestField { file, .. } => Some(file),
            Self::Ask(_) => None,
        }
    }
}

/// Index of a signal in its provider's `signals` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalId(pub usize);

/// What a manifest declared about a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared {
    /// A provider variant identified by its observation hook.
    Variant(String),
    /// The manifest names the provider without pinning a version.
    Named,
    /// A version or version constraint.
    Version(String),
    /// The manifest names a different provider for the same job.
    Alternative(ProviderId),
}
