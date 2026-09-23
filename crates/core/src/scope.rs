//! The workspace member a fact belongs to.

use std::path::PathBuf;

/// The workspace member a piece of evidence or a task belongs to, or the root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// The project root.
    Root,
    /// A workspace member.
    Member {
        /// The member's name.
        name: String,
        /// The member's directory.
        dir: PathBuf,
    },
}

impl Scope {
    /// The scope's label as `run` spells it.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Root => "root",
            Self::Member { name, .. } => name,
        }
    }
}
