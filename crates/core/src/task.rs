//! A runnable task.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::provider::ProviderId;
use crate::scope::Scope;

/// A task a provider declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The name as the source spells it.
    pub name: String,
    /// The provider that declared it.
    pub source: ProviderId,
    /// The workspace member it belongs to.
    pub scope: Scope,
    /// The tool's execution target when it differs from the name.
    pub target: Option<String>,
    /// The description the source gives.
    pub description: Option<String>,
    /// The task this one is an alias of.
    pub alias_of: Option<String>,
    /// The provider a one-line body hands the same name to.
    pub forwards_to: Option<ProviderId>,
    /// Everything else the source declared.
    pub detail: TaskDetail,
}

/// Structured facts about a task beyond name and description.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskDetail {
    /// Source configuration that declared the task.
    pub source: Option<PathBuf>,
    /// Tasks that run before this one.
    pub depends: Vec<String>,
    /// Tasks that run after this one.
    pub depends_post: Vec<String>,
    /// Tasks this one waits for when they are already scheduled.
    pub wait_for: Vec<String>,
    /// The directory the tool runs the task in.
    pub dir: Option<PathBuf>,
    /// `KEY=VALUE` pairs the task sets.
    pub env: Vec<String>,
    /// Tool versions the task pins.
    pub tools: BTreeMap<String, String>,
    /// The argument spec in the tool's own language.
    pub usage: Option<String>,
    /// The script file backing the task.
    pub file: Option<String>,
    /// Input globs.
    pub sources: Vec<String>,
    /// Output globs.
    pub outputs: Vec<String>,
    /// Timeout in the tool's own syntax.
    pub timeout: Option<String>,
}
