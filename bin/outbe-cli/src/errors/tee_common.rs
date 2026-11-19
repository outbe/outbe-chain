//! Typed errors for read-only TEE status.

#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeStatusError {
    #[error("critical renewal margin cannot exceed warning margin")]
    CriticalMarginExceedsWarning,
    #[error("read discovery-secret: {0}")]
    ReadDiscoverySecret(#[source] eyre::Report),
    #[error("discovery-secret is not a canonical Reth P2P private key: {0}")]
    InvalidDiscoverySecret(#[source] eyre::Report),
    #[error("encrypted reth key backend requires --reth-passphrase")]
    MissingRethPassphrase,
    #[error("reth passphrase requires the encrypted key backend")]
    PassphraseRequiresEncryptedBackend,
    #[error("unknown reth key backend: {0}")]
    UnknownRethKeyBackend(String),
    #[error("consensus status has no enclave health")]
    MissingEnclaveHealth,
}
