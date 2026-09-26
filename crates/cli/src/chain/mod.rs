//! Chain types and execution. v1 supports sequential + parallel chains
//! of task names plus the synthetic `Install` head used by
//! `runner install <tasks>`. v2 (out of scope here) will populate
//! `ChainItem.args` from a quoted-bundle parser.

pub(crate) mod exec;
pub(crate) mod mux;
pub(crate) mod parse;

/// A user-requested chain of tasks plus the policy that governs how
/// the chain reacts to per-task failures.
#[derive(Debug, Clone)]
pub(crate) struct Chain {
    pub mode: ChainMode,
    pub items: Vec<ChainItem>,
    pub failure: FailurePolicy,
}

/// Execution mode for the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainMode {
    Sequential,
    Parallel,
}

/// A single entry in a chain. v1 always sets `args` to an empty vec;
/// v2 quoted-bundle support will populate it from the parser.
#[derive(Debug, Clone)]
pub(crate) struct ChainItem {
    pub kind: ChainItemKind,
    pub args: Vec<String>,
}

impl ChainItem {
    /// Construct a chain item that dispatches the user-supplied task name.
    pub(crate) fn task(name: impl Into<String>) -> Self {
        Self {
            kind: ChainItemKind::Task(name.into()),
            args: Vec::new(),
        }
    }

    /// The synthetic install head `runner install <tasks>` runs first.
    pub(crate) const fn install() -> Self {
        Self {
            kind: ChainItemKind::Install,
            args: Vec::new(),
        }
    }

    /// Human-readable label for prefix-muxer output and error messages.
    pub(crate) const fn display_name(&self) -> &str {
        match &self.kind {
            ChainItemKind::Task(name) => name.as_str(),
            ChainItemKind::Install => "install",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ChainItemKind {
    /// User-supplied task name, resolved per-item via the existing 8-step chain.
    Task(String),
    /// Synthetic head used by `runner install <tasks>`.
    Install,
}

/// What a chain does after one of its tasks fails.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FailurePolicy {
    /// Keep starting the remaining tasks.
    Continue,
    /// Start no more tasks and let running ones finish.
    #[default]
    Wait,
    /// Start no more tasks and stop running ones.
    Kill,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_item_carries_empty_args_in_v1() {
        let item = ChainItem::task("build");
        assert_eq!(item.args.len(), 0);
        assert!(matches!(item.kind, ChainItemKind::Task(ref n) if n == "build"));
    }

    #[test]
    fn install_head_has_no_args() {
        let item = ChainItem::install();
        assert_eq!(item.args.len(), 0);
        assert!(matches!(item.kind, ChainItemKind::Install));
    }

    #[test]
    fn failure_policy_default_is_wait() {
        assert_eq!(FailurePolicy::default(), FailurePolicy::Wait);
    }

    #[test]
    fn display_name_is_install_for_install_head() {
        assert_eq!(ChainItem::install().display_name(), "install");
    }
}
