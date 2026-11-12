//! Typed errors for development session identity verification across reconnects.

use thiserror::Error;

/// Identity mismatch reasons when a development enclave session reconnects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum DevIdentityMismatchError {
    /// Reconnected enclave reported different SGX measurements.
    #[error("enclave measurements changed across reconnect")]
    MeasurementsChanged,

    /// Reconnected enclave reported a different Ed25519 attestation key.
    #[error("enclave attestation key changed across reconnect")]
    AttestationKeyChanged,

    /// Reconnected enclave reported a different Noise static key.
    #[error("enclave Noise static key changed across reconnect")]
    NoiseStaticKeyChanged,

    /// Reconnected enclave reported a different attestation environment label.
    #[error("enclave attestation environment changed across reconnect")]
    AttestationEnvironmentChanged,
}

impl DevIdentityMismatchError {
    /// Return the static error string for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MeasurementsChanged => "enclave measurements changed across reconnect",
            Self::AttestationKeyChanged => "enclave attestation key changed across reconnect",
            Self::NoiseStaticKeyChanged => "enclave Noise static key changed across reconnect",
            Self::AttestationEnvironmentChanged => {
                "enclave attestation environment changed across reconnect"
            }
        }
    }
}

impl std::ops::Deref for DevIdentityMismatchError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for DevIdentityMismatchError {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<DevIdentityMismatchError> for String {
    fn from(error: DevIdentityMismatchError) -> Self {
        error.as_str().to_owned()
    }
}
