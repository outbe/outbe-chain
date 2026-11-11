//! Errors of the kernel TSM report interface and platform attestation sources.

use std::fmt;

/// Error from the Linux TSM report interface (`configfs-tsm`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TsmError {
    /// A file system operation on a TSM report entry failed.
    #[error("{operation} {path}: {detail}")]
    Io {
        operation: &'static str,
        path: String,
        detail: String,
    },
    /// The kernel returned an empty report.
    #[error("TSM report {path} is empty")]
    EmptyReport { path: String },
    /// The report or its auxiliary data exceeds the accepted size.
    #[error("TSM {what} is {len} bytes, the limit is {max}")]
    TooLarge {
        what: &'static str,
        len: usize,
        max: usize,
    },
    /// The TSM provider is not one this service can attest with.
    #[error("unsupported TSM provider: {0}")]
    UnsupportedProvider(String),
    /// The report does not have the layout of its platform.
    #[error("malformed {platform} report: {detail}")]
    MalformedReport {
        platform: &'static str,
        detail: String,
    },
}

/// Error of the confidential VM backend of the service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfidentialVmError {
    /// A hook ran before `--platform` selected the backend.
    #[error("the confidential VM backend is not configured")]
    NotConfigured,
    /// The backend was selected two times in one process.
    #[error("the confidential VM backend is already configured")]
    AlreadyConfigured,
    /// The kernel has no TSM report interface.
    #[error("the kernel TSM report interface is absent; this is not a confidential VM")]
    TsmUnavailable,
    /// The processor did not return a derived key.
    #[error("SEV-SNP derived key request failed: {0}")]
    DerivedKey(String),
    /// The network descriptor file is unreadable or not canonical.
    #[error("network descriptor {path}: {detail}")]
    NetworkDescriptor { path: String, detail: String },
    /// A new report carries a launch identity other than the one read at startup.
    #[error("the report carries a launch identity other than the startup identity")]
    ReportIdentityChanged,
    /// The report does not bind the requested report data.
    #[error("the report does not bind the requested report data")]
    ReportDataMismatch,
    /// The blob was not sealed by the backend of this platform.
    #[error("the blob is not a sealed blob of this confidential VM platform")]
    NotSevSnpSeal,
    /// The sealed header names another key policy.
    #[error("the confidential VM sealed blob names another key policy")]
    SealPolicyMismatch,
    /// `--platform` was passed without `--network-descriptor`.
    #[error("--platform sev-snp and --platform tdx require --network-descriptor <file>")]
    MissingNetworkDescriptorArgument,
    /// The trust domain runs in debug mode.
    #[error("the trust domain runs in debug mode; the host can read its memory")]
    DebugTrustDomain,
    /// The dstack guest agent did not return a key.
    #[error("dstack guest agent key request failed: {0}")]
    GuestAgentKey(String),
    /// `--platform` names a platform that this binary does not support.
    #[error("unsupported --platform value: {0}")]
    UnsupportedPlatform(String),
}

impl ConfidentialVmError {
    /// Wrap a guest agent key request failure.
    pub fn guest_agent_key(err: impl fmt::Display) -> Self {
        Self::GuestAgentKey(err.to_string())
    }

    /// Wrap a derived key request failure.
    pub fn derived_key(err: impl fmt::Display) -> Self {
        Self::DerivedKey(err.to_string())
    }

    /// Wrap a network descriptor failure with its path.
    pub fn network_descriptor(path: impl fmt::Display, err: impl fmt::Display) -> Self {
        Self::NetworkDescriptor {
            path: path.to_string(),
            detail: err.to_string(),
        }
    }
}

impl TsmError {
    /// Wrap a file system error with the operation and the path.
    pub fn io(operation: &'static str, path: impl fmt::Display, err: impl fmt::Display) -> Self {
        Self::Io {
            operation,
            path: path.to_string(),
            detail: err.to_string(),
        }
    }
}
