//! Typed error definitions for CLI parsing and validation in `outbe-chain`.

/// Represents errors from CLI argument parsing and ChainSpec validation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError {
    /// Indicates that the RPC module list could not be parsed.
    #[error("Failed to parse RPC modules: {details}")]
    ParseRpcModules {
        /// Parser failure details.
        details: String,
    },
    /// Indicates an unrecognized RPC module name.
    #[error("Unknown RPC module: '{name}'")]
    UnknownRpcModule {
        /// Unrecognized module name.
        name: String,
    },
    /// Indicates an invalid consensus chain identity.
    #[error("invalid consensus chain identity: {details}")]
    InvalidConsensusChainIdentity {
        /// Identity failure details.
        details: String,
    },
    /// Indicates an unknown Outbe chain ID.
    #[error("unknown Outbe chain ID {chain_id}")]
    UnknownOutbeChainId {
        /// Unsupported chain ID.
        chain_id: u64,
    },
    /// Indicates an invalid mandatory `teeAttestationV1` ChainSpec configuration.
    #[error("invalid mandatory teeAttestationV1 ChainSpec: {details}")]
    InvalidMandatoryTeeChainSpec {
        /// Validation failure details.
        details: String,
    },
    /// Indicates an invalid `config.outbeProtocol` genesis block section.
    #[error("invalid config.outbeProtocol: {details}")]
    InvalidOutbeProtocolConfig {
        /// Validation failure details.
        details: String,
    },
    /// Indicates that the encrypted BLS key backend lacks a passphrase.
    #[error("--bls-key-backend encrypted requires --bls-passphrase or BLS_PASSPHRASE env var")]
    MissingBlsPassphrase,
    /// Indicates an unknown BLS key backend name.
    #[error("unknown BLS key backend: {backend}")]
    UnknownBlsKeyBackend {
        /// Unrecognized backend name.
        backend: String,
    },
    /// Indicates that the encrypted EVM key backend lacks a passphrase.
    #[error("--evm-key-backend encrypted requires --evm-passphrase or EVM_PASSPHRASE env var")]
    MissingEvmPassphrase,
    /// Indicates that the encrypted Reth key backend lacks a passphrase.
    #[error(
        "--reth-key-backend encrypted requires --reth-passphrase or RETH_P2P_PASSPHRASE env var"
    )]
    MissingRethPassphrase,
    /// Indicates an unknown EVM key backend name.
    #[error("unknown EVM key backend: {backend}")]
    UnknownEvmKeyBackend {
        /// Unrecognized backend name.
        backend: String,
    },
    /// Indicates an unknown Reth key backend name.
    #[error("unknown Reth key backend: {backend}")]
    UnknownRethKeyBackend {
        /// Unrecognized backend name.
        backend: String,
    },
    /// Indicates that a compressed public key failed hex decoding.
    #[error("expected 33-byte compressed public key: {0}")]
    InvalidCompressedPublicKey(#[from] hex::FromHexError),
    /// Indicates that node CLI inputs failed to parse during snapshot validation.
    #[error("{details}")]
    NodeInputParseFailure {
        /// Formatted parse error details.
        details: String,
    },
    /// Indicates that snapshot validation reported failed or incomplete checks.
    #[error("snapshot validation contains failed or incomplete checks")]
    SnapshotValidationFailed,
}
