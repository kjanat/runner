//! Whether a plan can fetch.

use crate::policy::ReachPolicy;

/// Whether a command can reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reach {
    /// Runs from what is on disk.
    Local,
    /// May fetch.
    Network,
}

/// Apply the reach policy to one command. Confirmation belongs to the caller's UI.
pub fn permitted(reach: Reach, policy: ReachPolicy, confirm: impl FnOnce() -> bool) -> bool {
    reach == Reach::Local
        || match policy {
            ReachPolicy::Allow => true,
            ReachPolicy::Local => false,
            ReachPolicy::Ask => confirm(),
        }
}

#[cfg(test)]
mod tests {
    use super::{Reach, ReachPolicy, permitted};

    #[test]
    fn only_an_ask_for_network_needs_confirmation() {
        for policy in [ReachPolicy::Ask, ReachPolicy::Allow, ReachPolicy::Local] {
            assert!(permitted(Reach::Local, policy, || panic!(
                "local command prompted"
            )));
        }
        assert!(permitted(Reach::Network, ReachPolicy::Allow, || panic!(
            "allow prompted"
        )));
        assert!(!permitted(Reach::Network, ReachPolicy::Local, || panic!(
            "local policy prompted"
        )));
        assert!(permitted(Reach::Network, ReachPolicy::Ask, || true));
        assert!(!permitted(Reach::Network, ReachPolicy::Ask, || false));
    }
}
