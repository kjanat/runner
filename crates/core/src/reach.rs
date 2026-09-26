//! Whether a plan can fetch.

use crate::policy::Download;

/// Whether a command can reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reach {
    /// Runs from what is on disk.
    Local,
    /// May fetch.
    Network,
}

/// Apply the download policy to one command. Confirmation belongs to the caller's UI.
pub fn permitted(reach: Reach, policy: Download, confirm: impl FnOnce() -> bool) -> bool {
    reach == Reach::Local
        || match policy {
            Download::Allow => true,
            Download::Refuse => false,
            Download::Ask => confirm(),
        }
}

#[cfg(test)]
mod tests {
    use super::{Download, Reach, permitted};

    #[test]
    fn only_an_ask_for_network_needs_confirmation() {
        for policy in [Download::Ask, Download::Allow, Download::Refuse] {
            assert!(permitted(Reach::Local, policy, || panic!(
                "local command prompted"
            )));
        }
        assert!(permitted(Reach::Network, Download::Allow, || panic!(
            "allow prompted"
        )));
        assert!(!permitted(Reach::Network, Download::Refuse, || panic!(
            "refuse prompted"
        )));
        assert!(permitted(Reach::Network, Download::Ask, || true));
        assert!(!permitted(Reach::Network, Download::Ask, || false));
    }
}
