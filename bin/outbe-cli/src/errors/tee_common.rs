//! Typed errors for read-only TEE status.

#[derive(Debug, thiserror::Error)]
pub(crate) enum DiscoverySecretError {
    #[error("discovery-secret IO failed")]
    Io(#[source] std::io::Error),
    #[error("discovery-secret hex rejected")]
    Hex(#[source] hex::FromHexError),
    #[error("discovery-secret secp256k1 key rejected")]
    Secp256k1(#[source] k256::ecdsa::Error),
    #[error(transparent)]
    Bls(#[from] outbe_simplex::errors::BlsKeyError),
    #[error("discovery-secret custody rejected")]
    Custody(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("discovery-secret must be a regular file")]
    NotRegularFile,
    #[error("discovery-secret exceeds the read limit")]
    TooLarge,
    #[error("discovery-secret must be 64 ASCII hex bytes")]
    PlaintextLength,
    #[error("compressed SEC1 public key must be 33 bytes")]
    CompressedLength,
    #[error("discovery-secret format does not match the requested key backend")]
    BackendFormat,
}

impl TeeStatusError {
    pub(crate) fn read_discovery_io(source: std::io::Error) -> Self {
        Self::ReadDiscoverySecret(DiscoverySecretError::Io(source))
    }

    pub(crate) fn invalid_backend_format() -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::BackendFormat)
    }

    pub(crate) fn invalid_not_regular_file() -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::NotRegularFile)
    }

    pub(crate) fn invalid_too_large() -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::TooLarge)
    }

    pub(crate) fn invalid_plaintext_length() -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::PlaintextLength)
    }

    pub(crate) fn invalid_hex(source: hex::FromHexError) -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::Hex(source))
    }

    pub(crate) fn invalid_secp256k1(source: k256::ecdsa::Error) -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::Secp256k1(source))
    }

    pub(crate) fn invalid_compressed_length() -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::CompressedLength)
    }

    pub(crate) fn invalid_insecure_mode(path: &std::path::Path, mode: u32) -> Self {
        Self::InvalidDiscoverySecret(DiscoverySecretError::Bls(
            outbe_simplex::errors::BlsKeyError::InsecureFileMode {
                kind: "secp256k1 secret".to_owned(),
                path: path.display().to_string(),
                mode,
            },
        ))
    }

    pub(crate) fn from_custody_report(error: eyre::Report) -> Self {
        let missing = error.chain().any(|item| {
            item.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
        });
        let typed = discovery_secret_error(error);
        if missing {
            Self::ReadDiscoverySecret(typed)
        } else {
            Self::InvalidDiscoverySecret(typed)
        }
    }
}

fn discovery_secret_error(error: eyre::Report) -> DiscoverySecretError {
    match error.downcast::<std::io::Error>() {
        Ok(io) => DiscoverySecretError::Io(io),
        Err(error) => match error.downcast::<hex::FromHexError>() {
            Ok(hex_error) => DiscoverySecretError::Hex(hex_error),
            Err(error) => match error.downcast::<k256::ecdsa::Error>() {
                Ok(secp) => DiscoverySecretError::Secp256k1(secp),
                Err(error) => match error.downcast::<outbe_simplex::errors::BlsKeyError>() {
                    Ok(bls) => DiscoverySecretError::Bls(bls),
                    Err(error) => DiscoverySecretError::Custody(error.into()),
                },
            },
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeStatusError {
    #[error("critical renewal margin cannot exceed warning margin")]
    CriticalMarginExceedsWarning,
    #[error("read discovery-secret: {0}")]
    ReadDiscoverySecret(#[source] DiscoverySecretError),
    #[error("discovery-secret is not a canonical Reth P2P private key: {0}")]
    InvalidDiscoverySecret(#[source] DiscoverySecretError),
    #[error("encrypted reth key backend requires --reth-passphrase")]
    MissingRethPassphrase,
    #[error("reth passphrase requires the encrypted key backend")]
    PassphraseRequiresEncryptedBackend,
    #[error("unknown reth key backend: {0}")]
    UnknownRethKeyBackend(String),
    #[error("consensus status has no enclave health")]
    MissingEnclaveHealth,
}
