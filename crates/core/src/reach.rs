//! Whether a plan can fetch.

/// Whether a command can reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reach {
    /// Runs from what is on disk.
    Local,
    /// May fetch.
    Network,
}
