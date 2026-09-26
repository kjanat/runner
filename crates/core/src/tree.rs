//! The directory runner was invoked for.

use std::path::PathBuf;

use crate::scope::Scope;

/// The invocation directory, its project root and the workspace members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    /// Where runner was invoked.
    pub cwd: PathBuf,
    /// The workspace root when `cwd` sits inside one, else `cwd`.
    pub root: PathBuf,
    /// Every workspace member.
    pub members: Vec<Scope>,
}
