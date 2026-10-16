//! Defines core engine error types for args, follow transport, peer management, and validators.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Represents errors from application shutdown synchronization.
#[derive(Debug, Error)]
pub enum ApplicationShutdownError {
    /// Indicates that the shutdown drain failed.
    #[error("{0}")]
    DrainFailed(String),
    /// Indicates that the consensus stack panicked.
    #[error("consensus stack panicked: {0}")]
    ConsensusStackPanicked(String),
}

/// Represents CLI argument validation errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArgsError {
    /// Indicates that DcapRequired policy rejects the development enclave session.
    #[error("DcapRequired forbids the development enclave session")]
    DcapRequiredForbidsDevSession,
    /// Indicates that the pending staleness duration is zero.
    #[error("--txpool.outbe.pending-staleness-secs must be greater than zero")]
    PendingStalenessSecsZero,
    /// Indicates that follower mode and validator mode are both enabled.
    #[error("--upstream (follower mode) is mutually exclusive with --validator")]
    UpstreamMutuallyExclusiveWithValidator,
    /// Indicates that nocertify mode lacks an upstream endpoint.
    #[error("--upstream.nocertify requires --upstream")]
    UpstreamNocertifyRequiresUpstream,
    /// Indicates that validator mode lacks a consensus signing key path.
    #[error(
        "--validator requires --consensus.signing-key. Provide the path to your BLS signing key file."
    )]
    ValidatorRequiresSigningKey,
    /// Indicates that validator mode lacks a Radicle control socket.
    #[error("--validator requires --radicle.control-socket")]
    ValidatorRequiresRadicleControlSocket,
    /// Indicates that validator mode lacks a Radicle status address.
    #[error("--validator requires --radicle.status-address")]
    ValidatorRequiresRadicleStatusAddress,
    /// Indicates that the Radicle status address is not a loopback address.
    #[error("--radicle.status-address must be loopback-only")]
    RadicleStatusAddressNotLoopback,
    /// Indicates that the caller specified Radicle flags without validator mode.
    #[error("Radicle flags require --validator")]
    RadicleFlagsRequireValidator,
    /// Indicates an incomplete manual DKG flag combination.
    #[error(
        "manual DKG provisioning requires either all of --consensus.signing-share, --consensus.public-polynomial, --consensus.dkg-output (signer), or --consensus.public-polynomial + --consensus.dkg-output without --consensus.signing-share (verifier-join)."
    )]
    InvalidManualDkgProvisioning,
    /// Indicates that the encrypted BLS key backend lacks a passphrase.
    #[error("--bls-key-backend encrypted requires --bls-passphrase or BLS_PASSPHRASE env var.")]
    EncryptedBackendRequiresPassphraseArg,
    /// Indicates that default EVM key derivation lacks a consensus signing key.
    #[error(
        "--validator requires --consensus.signing-key before deriving default --validator.evm-key"
    )]
    ValidatorEvmKeyRequiresSigningKey,
    /// Indicates that the encrypted BLS backend lacks a passphrase value.
    #[error("encrypted backend requires passphrase")]
    EncryptedBackendRequiresPassphrase,
    /// Indicates an unrecognized BLS key backend name.
    #[error("unknown BLS key backend: {0} (expected: plaintext, encrypted, os-level)")]
    UnknownBlsKeyBackend(String),
}

/// Represents errors from upstream follow transport initialization and RPC queries.
#[derive(Debug, Error)]
pub enum FollowTransportError {
    /// Indicates that building the upstream RPC client failed.
    #[error("failed to build upstream RPC client for {url}: {error}")]
    BuildUpstreamRpcClient {
        /// Normalized upstream URL.
        url: String,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that the upstream eth_call for tributeOfferPublicKey failed.
    #[error("upstream eth_call tributeOfferPublicKey failed: {0}")]
    UpstreamTributeOfferPublicKeyCall(String),
    /// Indicates that the upstream eth_call returned malformed hex data.
    #[error("malformed eth_call result from upstream: {0}")]
    MalformedEthCallResult(String),
    /// Indicates that tributeOfferPublicKey returned an unexpected byte length.
    #[error("upstream tributeOfferPublicKey returned {0} bytes instead of one ABI word")]
    UnexpectedTributeOfferPublicKeyLength(usize),
}

/// Represents errors from follower ingress shutdown coordination.
#[derive(Debug, Error)]
pub enum FollowerShutdownError {
    /// Indicates a poisoned follower ingress mutex.
    #[error("follower ingress lock poisoned")]
    IngressLockPoisoned,
    /// Indicates that the follower execution drain exceeded its deadline.
    #[error("follower execution/proof drain deadline exceeded before Marshal stop")]
    DrainDeadlineExceeded,
    /// Indicates that the follower execution drain ended without completion.
    #[error("follower execution/proof drain ended without completion")]
    DrainEndedWithoutCompletion,
    /// Indicates that the caller installed the follower ingress handle twice.
    #[error("follower ingress already installed")]
    IngressAlreadyInstalled,
    /// Indicates that the follower drained before receiving a shutdown request.
    #[error("follower drained without a shutdown request")]
    DrainedWithoutShutdownRequest,
    /// Indicates that the follower drain owner dropped its channel unexpectedly.
    #[error("follower drain owner disappeared")]
    DrainOwnerDisappeared,
}

/// Represents errors from validator registry and TEE state queries.
#[derive(Debug, Error)]
pub enum ValidatorsError {
    /// Indicates that reading Reth state storage failed.
    #[error("reth storage read failed: {0}")]
    RethStorageRead(String),
    /// Indicates that TEE runtime admission lacked a complete finalized block context.
    #[error("TEE runtime admission requires a complete finalized block context")]
    TeeRuntimeAdmissionIncompleteContext,
    /// Indicates that reading the local NodeHost TEE binding failed.
    #[error("read local NodeHost TEE binding: {0}")]
    ReadLocalNodeHostTeeBinding(String),
    /// Indicates that reading active validators failed.
    #[error("failed to read active validators: {0}")]
    ReadActiveValidators(String),
    /// Indicates that reading the active consensus set failed.
    #[error("failed to read active consensus set: {0}")]
    ReadActiveConsensusSet(String),
    /// Indicates that reading the reshare target set failed.
    #[error("failed to read reshare target set: {0}")]
    ReadReshareTargetSet(String),
    /// Indicates that reading pending validators failed.
    #[error("failed to read pending validators: {0}")]
    ReadPendingValidators(String),
    /// Indicates that reading admitted non-consensus validators failed.
    #[error("failed to read admitted non-consensus validators: {0}")]
    ReadAdmittedNonConsensusValidators(String),
    /// Indicates that a validator has an invalid BLS public key.
    #[error("invalid BLS pubkey for {validator}: {error}")]
    InvalidBlsPubkey {
        /// Validator EVM address.
        validator: Address,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that a hostname failed Commonware ingress parsing.
    #[error("invalid commonware hostname: {0}")]
    InvalidCommonwareHostname(String),
    /// Indicates that reading state at a specific block hash failed.
    #[error("failed to get state at block {block_hash}: {error}")]
    GetStateAtBlock {
        /// Block hash.
        block_hash: B256,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that reading the latest state provider failed.
    #[error("failed to get latest state: {0}")]
    GetLatestState(String),
    /// Indicates that reading the committee snapshot for an epoch failed.
    #[error("failed to read committee snapshot for epoch {epoch}: {error}")]
    ReadCommitteeSnapshot {
        /// Epoch number.
        epoch: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that checking a pending validator set change failed.
    #[error("failed to check pending set change: {0}")]
    CheckPendingSetChange(String),
    /// Indicates that reading the TEE offer public key failed.
    #[error("failed to read tee offer public key: {0}")]
    ReadTeeOfferPublicKey(String),
    /// Indicates that reading the TEE offer epoch failed.
    #[error("failed to read tee offer epoch: {0}")]
    ReadTeeOfferEpoch(String),
    /// Indicates that reading the validator TEE recipient X25519 key failed.
    #[error("failed to read tee recipient_x25519: {0}")]
    ReadTeeRecipientX25519(String),
    /// Indicates that reading the local validator TEE binding failed.
    #[error("read local validator TEE binding: {0}")]
    ReadLocalValidatorTeeBinding(String),
}

/// Builds a precompile revert error when a validator lacks a NodeHost enclave binding.
pub fn validator_has_no_node_host_enclave_binding_error() -> PrecompileError {
    PrecompileError::Revert("validator has no NodeHost enclave binding".into())
}

/// Represents errors from peer manager actor, admission, and ingress operations.
#[derive(Debug, Error)]
pub enum PeerManagerError {
    /// Indicates that reading the provider block hash at a height failed.
    #[error("failed to read provider block hash at {height}: {error}")]
    ReadProviderBlockHash {
        /// Block height.
        height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that the provider has no canonical hash at the finalized height.
    #[error("provider has no canonical hash at finalized block height {0}")]
    MissingCanonicalHashAtFinalizedHeight(u64),
    /// Indicates a hash mismatch between the provider and consensus at a finalized height.
    #[error(
        "provider hash mismatch at height {height}: provider={provider_hash}, consensus={consensus_hash}"
    )]
    ProviderHashMismatch {
        /// Block height.
        height: u64,
        /// Provider hash.
        provider_hash: B256,
        /// Consensus hash.
        consensus_hash: B256,
    },
    /// Indicates that the peer-set index counter overflowed.
    #[error("peer-set index exhausted")]
    PeerSetIndexExhausted,
    /// Indicates that a P2P peer-set index did not increase monotonically.
    #[error("non-increasing P2P peer-set index")]
    NonIncreasingPeerSetIndex,
    /// Indicates that the P2P oracle channel closed.
    #[error("P2P oracle closed")]
    P2pOracleClosed,
    /// Indicates that the peer manager mailbox channel closed.
    #[error("peer_manager mailbox closed")]
    MailboxClosed,
    /// Indicates that the peer manager dropped a DKG publication response.
    #[error("peer_manager dropped DKG publication")]
    DroppedDkgPublication,
}
