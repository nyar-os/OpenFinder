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
