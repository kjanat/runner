//! What a provider's health check reports.

/// The parsed result of a provider's health command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Nothing to report.
    Ok,
    /// The tool's own messages, one per problem.
    Problems(Vec<String>),
    /// The output could not be read.
    Unreadable(String),
}
