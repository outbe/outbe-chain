//! Typed errors for read-only TEE status.

#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeStatusError {
    #[error("critical renewal margin cannot exceed warning margin")]
    CriticalMarginExceedsWarning,
    #[error("read public NodeHost manifest: {0}")]
    ReadPublicManifest(#[source] std::io::Error),
    #[error("NodeHost manifest must be a regular file of at most 512 bytes")]
    InvalidPublicManifestFile,
    #[error("decode public NodeHost manifest: {0}")]
    DecodePublicManifest(#[source] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("committed NodeHost manifest chain identity does not match finalized policy")]
    ManifestChainIdentityMismatch,
    #[error("consensus status has no enclave health")]
    MissingEnclaveHealth,
}
