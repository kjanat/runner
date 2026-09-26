//! A non-fatal finding.

use crate::provider::ProviderId;

/// A finding the pipeline reports without stopping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Warning {
    /// The provider the warning is about, when there is one.
    pub provider: Option<ProviderId>,
    /// The message.
    pub message: String,
}

impl Warning {
    /// A warning about `provider`.
    #[must_use]
    pub fn about(provider: ProviderId, message: impl Into<String>) -> Self {
        Self {
            provider: Some(provider),
            message: message.into(),
        }
    }

    /// A warning about the project as a whole.
    #[must_use]
    pub fn general(message: impl Into<String>) -> Self {
        Self {
            provider: None,
            message: message.into(),
        }
    }
}
