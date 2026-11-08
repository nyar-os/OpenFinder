//! Stable domain error and capability state contract.
//!
//! The N-API boundary serialises [`Error::payload`] so the CLI, skills, and UI
//! share one machine-readable error shape. Do not add a second error schema.

use std::fmt;

use serde::Serialize;

use crate::platform::platform_name;

/// Why a capability is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    /// The target platform has no backend for this capability.
    UnsupportedPlatform,
    /// The platform backend exists but the application has not implemented it.
    NotImplemented,
    /// A required dependency or permission is unavailable.
    DependencyBlocked,
}

impl CapabilityState {
    /// Stable code written to errors and audit records.
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::NotImplemented => "not_implemented",
            Self::DependencyBlocked => "dependency_blocked",
        }
    }
}

impl fmt::Display for CapabilityState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// Domain error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A capability is unavailable for a structured reason.
    #[error("{capability} is unavailable ({state}): {detail}")]
    Capability {
        /// Stable capability id.
        capability: &'static str,
        /// Structured unavailability reason.
        state: CapabilityState,
        /// Human-readable detail.
        detail: String,
    },
    /// Filesystem failure at a known path.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path that failed.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A request refused by a declared policy, with a stable script-facing code.
    #[error("{code}: {detail}")]
    Policy {
        /// Stable code, for example `window_second_launch_rejected`.
        code: &'static str,
        /// Human-readable detail.
        detail: String,
    },
    /// Other domain failure.
    #[error("{0}")]
    Message(String),
}

impl Error {
    /// Construct a structured capability error.
    pub fn capability(capability: &'static str, state: CapabilityState, detail: impl Into<String>) -> Self {
        Self::Capability { capability, state, detail: detail.into() }
    }

    /// Construct a filesystem error.
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    /// Construct a plain domain error.
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    /// Construct a policy refusal with a stable code.
    pub fn policy(code: &'static str, detail: impl Into<String>) -> Self {
        Self::Policy { code, detail: detail.into() }
    }

    /// Machine-readable payload shared by the N-API boundary and the CLI.
    pub fn payload(&self) -> ErrorPayload {
        ErrorPayload {
            code: self.code().to_string(),
            capability: capability_code(self).map(str::to_string),
            platform: platform_name().to_string(),
            message: self.to_string(),
            path: match self {
                Self::Io { path, .. } => Some(path.clone()),
                _ => None,
            },
        }
    }

    /// Stable error code, also used as the audit `errorCode`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Capability { state, .. } => state.code(),
            Self::Io { .. } => "io_error",
            Self::Policy { code, .. } => code,
            Self::Message(_) => "message",
        }
    }
}

/// JSON payload for one domain error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    /// Stable code: a capability state, `io_error`, or `message`.
    pub code: String,
    /// Capability id when the error is a capability refusal.
    pub capability: Option<String>,
    /// Runtime platform identity for diagnostics.
    pub platform: String,
    /// Human-readable message.
    pub message: String,
    /// Path when the error came from filesystem access.
    pub path: Option<String>,
}

/// Capability id for a capability error.
pub fn capability_code(error: &Error) -> Option<&'static str> {
    match error {
        Error::Capability { capability, .. } => Some(capability),
        _ => None,
    }
}

/// Domain result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_states_have_stable_codes() {
        assert_eq!(CapabilityState::UnsupportedPlatform.code(), "unsupported_platform");
        assert_eq!(CapabilityState::NotImplemented.code(), "not_implemented");
        assert_eq!(CapabilityState::DependencyBlocked.code(), "dependency_blocked");
    }

    #[test]
    fn payload_carries_the_capability_code() {
        let error = Error::capability("index-build", CapabilityState::DependencyBlocked, "elevation required");
        let payload = error.payload();
        assert_eq!(payload.code, "dependency_blocked");
        assert_eq!(payload.capability.as_deref(), Some("index-build"));
        assert!(payload.path.is_none());
    }

    #[test]
    fn io_errors_report_the_path() {
        let error = Error::io("C:\\index\\meta.json", std::io::Error::from(std::io::ErrorKind::NotFound));
        let payload = error.payload();
        assert_eq!(payload.code, "io_error");
        assert_eq!(payload.path.as_deref(), Some("C:\\index\\meta.json"));
        assert!(payload.capability.is_none());
    }
}
