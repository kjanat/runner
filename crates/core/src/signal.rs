//! What a provider tells the core to look for.

use std::io;
use std::path::Path;

use crate::evidence::Evidence;
use crate::provider::ProviderId;

/// Something to look for in a scope directory.
///
/// A provider lists its signals in precedence order: among equally weighted
/// evidence, the earlier signal decides.
#[derive(Clone, Copy)]
pub enum Signal {
    /// A file in the scope directory.
    File(&'static str),
    /// A file in the scope directory, matched without regard to ASCII case.
    FileCaseless(&'static str),
    /// A file in the scope directory or an ancestor.
    FileUpwards(&'static str),
    /// A file in the scope directory whose text names the provider.
    FileContent {
        /// The file name.
        name: &'static str,
        /// Reads the text into a declaration, `None` when it does not name the provider.
        parse: fn(&str) -> Option<Declared>,
    },
    /// A file that also pins the provider.
    Lockfile(&'static str),
    /// A manifest field that names or constrains the provider.
    ManifestField {
        /// The manifest file names, in the order they are tried; the first one present is read.
        files: &'static [&'static str],
        /// Dotted path of the field.
        path: &'static str,
        /// Reads the field into a declaration, `None` when it names another provider.
        parse: fn(&Field<'_>) -> Option<Declared>,
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
            | Self::FileCaseless(name)
            | Self::FileUpwards(name)
            | Self::Lockfile(name)
            | Self::EnvVar(name)
            | Self::Probe(name)
            | Self::FileContent { name, .. } => Some(name),
            Self::ManifestField { files, .. } => files.first().copied(),
            Self::Ask(_) => None,
        }
    }

    /// Every file name the signal can match in a scope directory.
    #[must_use]
    pub fn file_names(&self) -> Vec<&'static str> {
        match self {
            Self::File(name)
            | Self::FileCaseless(name)
            | Self::FileUpwards(name)
            | Self::Lockfile(name)
            | Self::FileContent { name, .. } => vec![name],
            Self::ManifestField { files, .. } => files.to_vec(),
            Self::EnvVar(_) | Self::Probe(_) | Self::Ask(_) => Vec::new(),
        }
    }
}

/// A manifest field handed to a signal's parser, with the manifest it sits in.
#[derive(Debug, Clone, Copy)]
pub struct Field<'a> {
    /// The value at the signal's path.
    pub value: &'a serde_json::Value,
    /// The whole manifest.
    pub manifest: &'a serde_json::Value,
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
    /// A requirement the manifest asks the tool to enforce.
    Constraint {
        /// The version constraint, when the manifest pins one.
        version: Option<String>,
        /// What the manifest asks for when the requirement is not met.
        on_fail: OnFail,
    },
    /// The manifest names a different provider for the same job.
    Alternative(ProviderId),
}

impl Declared {
    /// The version or constraint the declaration carries.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Version(version) => Some(version),
            Self::Constraint { version, .. } => version.as_deref(),
            Self::Variant(_) | Self::Named | Self::Alternative(_) => None,
        }
    }

    /// How strongly the declaration selects the provider among declarations
    /// of equal weight, strongest first: a field that names the provider,
    /// then one that constrains it, then a variant derived from either.
    #[must_use]
    pub const fn rank(&self) -> u8 {
        match self {
            Self::Named | Self::Version(_) => 0,
            Self::Constraint { .. } => 1,
            Self::Variant(_) => 2,
            Self::Alternative(_) => 3,
        }
    }
}

/// What a manifest asks for when a requirement it declares is not met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnFail {
    /// Proceed silently.
    Ignore,
    /// Proceed with a warning.
    Warn,
    /// Refuse.
    Error,
}

impl OnFail {
    /// The lowercase spelling reports print.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}
