//! Error type shared by every Vibe Factory crate.

use std::time::Duration;

/// Convenient alias used across the framework.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Broad classification of an error, used to decide whether an operation
/// should be retried, escalated, or reported to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The provider rejected the request because of rate limiting (HTTP 429).
    RateLimited,
    /// Credentials are missing, invalid or expired (HTTP 401/403).
    AuthFailed,
    /// The request itself is malformed (HTTP 400).
    InvalidRequest,
    /// The conversation no longer fits in the model context window.
    ContextTooLong,
    /// The remote service failed (HTTP 5xx).
    ServerError,
    /// A network-level failure (DNS, TLS, timeout, connection reset).
    Network,
    /// A tool refused or failed to execute.
    Tool,
    /// A security policy blocked the operation.
    Denied,
    /// Persistence layer failure.
    Storage,
    /// Configuration is missing or invalid.
    Config,
    /// Workspace (git, filesystem) failure.
    Workspace,
    /// A plugin misbehaved.
    Plugin,
    /// The operation was cancelled by the user or a hook.
    Cancelled,
    /// Anything else.
    Other,
}

impl ErrorKind {
    /// Whether an operation failing with this kind is worth retrying.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::ServerError | Self::Network)
    }
}

/// The framework error type.
#[derive(Debug, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct Error {
    /// Classification of the failure.
    pub kind: ErrorKind,
    /// Human readable description.
    pub message: String,
    /// Optional hint on how long to wait before retrying.
    pub retry_after: Option<Duration>,
    /// Underlying cause, if any.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl Error {
    /// Build a new error of the given kind.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after: None,
            source: None,
        }
    }

    /// Attach an underlying cause.
    #[must_use]
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Attach a retry hint.
    #[must_use]
    pub fn with_retry_after(mut self, after: Duration) -> Self {
        self.retry_after = Some(after);
        self
    }

    /// Shortcut for [`ErrorKind::Other`].
    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other, message)
    }

    /// Shortcut for [`ErrorKind::Config`].
    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    /// Shortcut for [`ErrorKind::Denied`].
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Denied, message)
    }

    /// Shortcut for [`ErrorKind::Tool`].
    pub fn tool(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Tool, message)
    }

    /// Shortcut for [`ErrorKind::Storage`].
    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Storage, message)
    }

    /// Shortcut for [`ErrorKind::Workspace`].
    pub fn workspace(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Workspace, message)
    }

    /// Shortcut for [`ErrorKind::Plugin`].
    pub fn plugin(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Plugin, message)
    }

    /// Whether this error is worth retrying.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.kind.is_retryable()
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorKind::Storage, err.to_string()).with_source(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::new(ErrorKind::InvalidRequest, format!("JSON error: {err}")).with_source(err)
    }
}

impl From<toml::de::Error> for Error {
    fn from(err: toml::de::Error) -> Self {
        Self::new(ErrorKind::Config, format!("TOML error: {err}")).with_source(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_kinds() {
        assert!(ErrorKind::RateLimited.is_retryable());
        assert!(ErrorKind::Network.is_retryable());
        assert!(!ErrorKind::AuthFailed.is_retryable());
        assert!(!Error::denied("nope").is_retryable());
    }

    #[test]
    fn display_includes_kind_and_message() {
        let e = Error::config("missing key");
        assert_eq!(e.to_string(), "Config: missing key");
    }
}
