//! Typed errors for non-TEE CLI commands.

use std::path::PathBuf;

use alloy_primitives::Address;

/// Errors for shared CLI command inputs.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CommandInputError {
    /// The decimal amount string is invalid.
    #[error("invalid amount: {0}")]
    InvalidAmount(alloy_primitives::ruint::ParseError),
    /// The parsed amount is zero.
    #[error("amount must be non-zero")]
    ZeroAmount,
    /// The command requires the `--private-key` flag.
    #[error("--private-key required for this operation")]
    PrivateKeyRequired,
}

/// Errors for the `epoch` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EpochError {
    /// The on-chain epoch number exceeds `u64::MAX`.
    #[error("epoch number exceeds u64")]
    EpochNumberOverflow,
}

/// Errors for the `monitor` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MonitorError {
    /// The on-chain epoch number exceeds `u64::MAX`.
    #[error("epoch number exceeds u64")]
    EpochNumberOverflow,
    /// The HTTP request to the metrics endpoint failed.
    #[error("failed to fetch metrics from {endpoint}: {source}")]
    MetricsFetchFailed {
        endpoint: String,
        source: reqwest::Error,
    },
    /// The metrics endpoint returned an unsuccessful HTTP status.
    #[error("metrics endpoint returned HTTP {0}")]
    MetricsHttpStatus(u16),
    /// Reading the response body from the metrics endpoint failed.
    #[error("failed to read metrics response: {0}")]
    MetricsReadFailed(reqwest::Error),
}

/// Errors for the `oracle` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OracleError {
    /// The asset argument is not `COEN`, an ISO 4217 code, or a hex address.
    #[error("{spec:?} is not COEN, an ISO 4217 code or a 0x address: {source}")]
    InvalidAsset {
        spec: String,
        source: alloy_primitives::hex::FromHexError,
    },
    /// The latest block response does not contain a timestamp field.
    #[error("latest block response is missing timestamp")]
    MissingBlockTimestamp,
    /// The block timestamp is not valid hexadecimal.
    #[error("failed to parse timestamp: {0}")]
    InvalidTimestamp(std::num::ParseIntError),
}

/// Errors for the `rad` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RadError {
    /// The Radicle home directory has no identity key file.
    #[error("no Radicle identity in {}: run `rad auth` first - it creates the keys, this command only rewrites the network settings", .0.display())]
    MissingIdentity(PathBuf),
    /// No validator has a registered Radicle NodeId on-chain.
    #[error("the chain reports no validators with a Radicle NodeId binding")]
    NoValidatorNodeIds,
    /// The `node` field in the configuration file is not a JSON object.
    #[error("`node` in {} is not an object", .0.display())]
    NodeConfigNotObject(PathBuf),
    /// Local decoding of the validator P2P address failed.
    #[error("validator {validator} has an undecodable P2P address: {source}")]
    UndecodableP2pAddress {
        validator: Address,
        source: outbe_primitives::consensus_p2p::P2pAddressError,
    },
    /// The raw Radicle NodeId does not have 32 bytes.
    #[error("Radicle NodeId must be 32 bytes, got {0}")]
    InvalidNodeIdLength(usize),
    /// The configuration file does not contain a top-level JSON object.
    #[error("{} must contain a JSON object", .0.display())]
    ConfigNotJsonObject(PathBuf),
    /// The Radicle configuration file does not exist.
    #[error("no config at {}: run `outbe-cli rad init` first", .0.display())]
    MissingConfig(PathBuf),
    /// The path for the Unix control socket exceeds the operating system limit.
    #[error("Radicle home is too deep: the control socket path {} would exceed the Unix socket limit - use a shorter --home", .0.display())]
    ControlSocketPathTooLong(PathBuf),
    /// The `kill` command failed to stop the running node process.
    #[error("failed to stop pid {0}")]
    StopProcessFailed(u32),
}

/// Errors for the `radicle` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RadicleError {
    /// The repository identifier is not valid hexadecimal.
    #[error("invalid RepoId hex: {0}")]
    InvalidRepoIdHex(hex::FromHexError),
    /// The decoded repository identifier does not have 20 bytes.
    #[error("RepoId must be exactly 20 bytes, got {0}")]
    InvalidRepoIdLength(usize),
}

/// Errors for the `slash` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SlashError {
    /// The decoded slashing evidence payload is empty.
    #[error("{0} evidence must be non-empty")]
    EmptyEvidence(&'static str),
}

/// Errors for the `stablecoin` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StablecoinError {
    /// The policy administrator address is zero.
    #[error("policy admin must not be the zero address")]
    ZeroPolicyAdmin,
}

/// Errors for the `tribute` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TributeError {
    /// The base amount is not a canonical unsigned 64-bit integer.
    #[error("amount must be a canonical unsigned u64")]
    InvalidAmountBase,
    /// The micro remainder is not a canonical integer below 1,000,000.
    #[error("amount_micro must be a canonical unsigned u64 below 1000000")]
    InvalidAmountMicro,
    /// The on-chain `TeeRegistry` has no bootstrapped offer key.
    #[error("TeeRegistry is not bootstrapped yet - no offer key to encrypt to")]
    TeeRegistryNotBootstrapped,
    /// Encrypting the tribute offer payload failed.
    #[error("offer encryption failed: {0}")]
    OfferEncryptionFailed(outbe_tee::LocalCryptoError),
    /// The command-line flag value is not valid hexadecimal.
    #[error("{flag} is not valid hex: {source}")]
    InvalidHexFlag {
        flag: String,
        source: hex::FromHexError,
    },
    /// The decoded `--zk-proof` byte slice is empty.
    #[error("--zk-proof must not be empty")]
    EmptyZkProof,
    /// The decoded 32-byte hex argument has the wrong length.
    #[error("{flag} must contain exactly 32 bytes, got {actual}")]
    InvalidHex32Length { flag: String, actual: usize },
}

/// Errors for the `validator` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ValidatorError {
    /// The OCOMP registration file is empty.
    #[error("OCOMP registration file is empty")]
    EmptyOcompRegistration,
    /// The caller combined `--symmetric` with asymmetric P2P options.
    #[error("--symmetric cannot be combined with asymmetric ingress/egress options")]
    SymmetricWithAsymmetricOptions,
    /// Asymmetric P2P configuration requires `--egress`.
    #[error("--egress is required for asymmetric p2p")]
    MissingAsymmetricEgress,
    /// A DNS ingress flag is missing its required companion flag.
    #[error("{missing} is required with {given}")]
    MissingIngressDnsCompanion {
        missing: &'static str,
        given: &'static str,
    },
    /// The caller did not select exactly one asymmetric ingress mode.
    #[error("set exactly one asymmetric ingress: --ingress-socket or --ingress-dns-host/--ingress-dns-port")]
    InvalidAsymmetricIngressSelection,
}

/// Errors for the `vote` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VoteError {
    /// The caller passed both `--yes` and `--no`.
    #[error("specify either --yes or --no, not both")]
    ConflictingVoteDirection,
    /// The caller passed neither `--yes` nor `--no`.
    #[error("specify --yes or --no")]
    MissingVoteDirection,
    /// The caller did not supply exactly one payload source.
    #[error("specify exactly one of --payload or --payload-file")]
    MissingOrDuplicatePayloadSource,
    /// The `votingWindowBlocks` field is not a positive integer.
    #[error("votingWindowBlocks must be a positive integer number of blocks")]
    InvalidVotingWindowBlocks,
    /// The RPC response for the finalized block has no `number` field.
    #[error("finalized block has no number")]
    FinalizedBlockMissingNumber,
    /// Decoding the finalized TEE policy failed.
    #[error("invalid finalized policy: {0}")]
    InvalidFinalizedPolicy(outbe_primitives::tee_attestation_v1::CodecError),
    /// The measurement rules for an enclave upgrade reject the update.
    #[error("enclave upgrade requires a different MRENCLAVE and one predecessor measurement")]
    InvalidEnclaveUpgradeMeasurement,
    /// The chain identifier in the TEE policy does not match the RPC chain identifier.
    #[error("TEE policy chain identity mismatch")]
    TeePolicyChainMismatch,
}

/// Errors for the `zero-fee` subcommand.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ZeroFeeError {
    /// The signer account has zero balance for bootstrap.
    #[error("ZeroFee bootstrap requires a positive balance (at least 0.000001 COEN)")]
    ZeroBootstrapBalance,
    /// Incrementing the account nonce for bootstrap authorization overflowed.
    #[error("bootstrap authorization nonce overflow")]
    BootstrapNonceOverflow,
    /// Signing the EIP-7702 authorization digest failed.
    #[error("EIP-7702 authorization signing failed: {0}")]
    AuthorizationSigningFailed(k256::ecdsa::Error),
}
