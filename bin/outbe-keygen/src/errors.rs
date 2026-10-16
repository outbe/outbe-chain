//! Typed errors for `outbe-keygen`.

use std::path::PathBuf;

/// Errors that `outbe-keygen` returns.
#[derive(Debug, thiserror::Error)]
pub enum KeygenError {
    /// The consensus BLS MinPk key argument is not 48 bytes of hex.
    #[error("consensus BLS MinPk key must be exactly 48 bytes of hex: {0}")]
    InvalidConsensusBlsMinPkHex(#[from] hex::FromHexError),
    /// The encrypted key backend requires a passphrase.
    #[error("--passphrase (or BLS_PASSPHRASE env) required for encrypted backend")]
    MissingPassphrase,
    /// The `blst` library rejected the secret key bytes.
    #[error("failed to create blst SecretKey: {0:?}")]
    BlstSecretKey(blst::BLST_ERROR),
    /// The Radicle NodeId is not 32 bytes long.
    #[error("Radicle NodeId is not a 32-byte Ed25519 key")]
    InvalidRadicleNodeIdLength,
    /// The EVM private key is not 32 bytes of hex.
    #[error("EVM private key must be exactly 32 bytes of hex")]
    InvalidEvmPrivateKeyHex,
    /// The EVM public key is not an uncompressed 65-byte SEC1 point.
    #[error("EVM public key is not an uncompressed SEC1-65 point")]
    InvalidEvmPublicKeyPoint,
    /// Validator key files already exist in the target directory.
    #[error("validator key artifacts already exist, refusing to overwrite: {0}")]
    ValidatorKeyArtifactsExist(String),
    /// The encoded BLS MinPk public key is not 48 bytes long.
    #[error("BLS MinPk public key is not 48 bytes")]
    InvalidBlsPublicKeyLength,
    /// The random number generator returned an all-zero ECDSA key.
    #[error("generated zero ECDSA key - extremely unlikely, try again")]
    ZeroEcdsaKey,
    /// The secret key path has no file name.
    #[error("secret key path has no file name: {0}")]
    SecretKeyPathMissingFileName(PathBuf),
}
