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
    /// The OCOMP output path is not a regular directory.
    #[error("OCOMP output path is not a regular directory: {0}")]
    OcompOutputNotRegularDir(PathBuf),
    /// An OCOMP file already exists at the target path.
    #[error("OCOMP artifact already exists: {0}")]
    OcompArtifactAlreadyExists(PathBuf),
    /// The generated OCOMP public key is not a 33-byte compressed SEC1 point.
    #[error("generated OCOMP public key is not compressed SEC1-33")]
    InvalidOcompPublicKeyLength,
    /// The signing key failed to sign the OCOMP proof of possession.
    #[error("failed to sign OCOMP proof of possession")]
    OcompPopSignFailed,
    /// The tool failed to write the OCOMP secret key and failed to remove the public file.
    #[error(
        "OCOMP secret installation failed at {key_path}: {error}; cleanup of the new public registration at {registration_path} also failed: {cleanup_error}"
    )]
    OcompSecretInstallAndCleanupFailed {
        key_path: PathBuf,
        error: String,
        registration_path: PathBuf,
        cleanup_error: std::io::Error,
    },
    /// The file path has no parent directory.
    #[error("artifact path has no parent: {0}")]
    ArtifactPathMissingParent(PathBuf),
    /// The file path has no file name.
    #[error("artifact path has no file name: {0}")]
    ArtifactPathMissingFileName(PathBuf),
    /// The secret key path has no file name.
    #[error("secret key path has no file name: {0}")]
    SecretKeyPathMissingFileName(PathBuf),
}
