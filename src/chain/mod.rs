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

    /// Construct the synthetic install-head used by `runner install <tasks>`.
    /// `frozen` mirrors the `--frozen` CLI flag and is propagated to the
    /// install executor (`npm ci`, `--frozen-lockfile`, etc.).
    pub(crate) const fn install(frozen: bool) -> Self {
        Self {
            kind: ChainItemKind::Install { frozen },
            args: Vec::new(),
        }
    }

    /// Human-readable label for prefix-muxer output and error messages.
    pub(crate) const fn display_name(&self) -> &str {
        match &self.kind {
            ChainItemKind::Task(name) => name.as_str(),
            ChainItemKind::Install { .. } => "install",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ChainItemKind {
    /// User-supplied task name, resolved per-item via the existing 8-step chain.
    Task(String),
    /// Synthetic head used by `runner install <tasks>`. Dispatches the
    /// detected PM's install command; `frozen` selects the lockfile-only
    /// install variant when true.
    Install { frozen: bool },
}

/// Failure policy for a chain. `FailFast` is the default and matches
/// `make -j` semantics in parallel mode (let running siblings finish,
/// don't start new ones).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum FailurePolicy {
    /// Stop the chain on the first failing task. In parallel mode,
    /// already-running siblings complete naturally.
    #[default]
    FailFast,
    /// Run every task to completion regardless of failures. Final exit
    /// code reflects the first failure.
    KeepGoing,
    /// Parallel only: SIGKILL siblings on first failure (`std::process::Child::kill`).
    /// Sequential callers accept this silently (no-op). Catch-able SIGTERM
    /// semantics would need a libc/nix dep — deferred to a follow-up.
    KillOnFail,
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
        let item = ChainItem::install(false);
        assert!(item.args.is_empty());
        assert!(matches!(
            item.kind,
            ChainItemKind::Install { frozen: false }
        ));
    }

    #[test]
    fn install_head_propagates_frozen_flag() {
        let item = ChainItem::install(true);
        assert!(matches!(item.kind, ChainItemKind::Install { frozen: true }));
    }

    #[test]
    fn failure_policy_default_is_fail_fast() {
        assert_eq!(FailurePolicy::default(), FailurePolicy::FailFast);
    }

    #[test]
    fn display_name_is_install_for_install_head() {
        assert_eq!(ChainItem::install(false).display_name(), "install");
    }
}
