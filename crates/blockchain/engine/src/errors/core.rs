//! Defines core engine error types for args, CE finalization, follow transport, peer management, and validators.

use std::time::Duration;

use alloy_primitives::{Address, B256};
use outbe_compressed_entities::FinalizedMarker;
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
    /// Indicates that the projection storage config path is missing.
    #[error("offchain storage is required; provide --projection.storage-config")]
    OffchainStorageRequired,
    /// Indicates that the projection storage config path is empty.
    #[error("--projection.storage-config must not be empty")]
    ProjectionStorageConfigEmpty,
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

/// Represents errors from the CE finalizer and durable persistence checks.
#[derive(Debug, Error)]
pub enum CeFinalizerError {
    /// Indicates that the genesis header contains a CE root artifact.
    #[error("genesis durable header unexpectedly carries a CE root artifact")]
    GenesisCarriesCeRootArtifact,
    /// Indicates that a canonical header lacks a CE root artifact.
    #[error("durable canonical header is missing CE root artifact at {height}/{block_hash}")]
    MissingCeRootArtifact {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates a CE scheme mismatch between the header and the active configuration.
    #[error(
        "durable canonical header CE scheme mismatch at {height}/{block_hash}: header={header}, active={active}"
    )]
    CeSchemeMismatch {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Header scheme id.
        header: u32,
        /// Active scheme id.
        active: u32,
    },
    /// Indicates a CE root mismatch between the header and EVM storage.
    #[error(
        "durable canonical header/EVM CE root mismatch at {height}/{block_hash}: header={header}, evm={evm}"
    )]
    HeaderEvmCeRootMismatch {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Header CE root.
        header: B256,
        /// EVM CE root.
        evm: B256,
    },
    /// Indicates that the Reth DB-only provider failed to open.
    #[error("failed to open Reth DB-only provider: {0}")]
    OpenDbProvider(String),
    /// Indicates that reading a durable block failed.
    #[error("failed to read durable block {height}: {error}")]
    ReadDurableBlock {
        /// Block height.
        height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that reading a durable header failed.
    #[error("failed to read durable header {height}: {error}")]
    ReadDurableHeader {
        /// Block height.
        height: u64,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that a durable header is missing.
    #[error("durable header {height}/{block_hash} is missing")]
    DurableHeaderMissing {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates a hash conflict between the height index and the durable header.
    #[error(
        "durable header/hash index conflict at height {height}: index={index}, header={header}"
    )]
    DurableHeaderHashConflict {
        /// Block height.
        height: u64,
        /// Index block hash.
        index: B256,
        /// Header block hash.
        header: B256,
    },
    /// Indicates that decoding durable header artifacts failed.
    #[error("failed to decode durable header artifacts for {height}/{block_hash}: {error}")]
    DecodeDurableHeaderArtifacts {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that reading the durable CE root failed.
    #[error("failed to read durable CE root for {height}/{block_hash}: {error}")]
    ReadDurableCeRoot {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that the durable parent checkpoint is missing.
    #[error("durable parent checkpoint {0} is missing")]
    DurableParentCheckpointMissing(u64),
    /// Indicates an attempt to replay height 0 as a normal block.
    #[error("height 0 is the CE replay baseline, not a replay block")]
    HeightZeroNotReplayBlock,
    /// Indicates that the durable canonical parent is unavailable during replay.
    #[error("durable canonical parent {parent_height} is unavailable while replaying {height}/{block_hash}")]
    DurableCanonicalParentUnavailable {
        /// Parent block height.
        parent_height: u64,
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates that opening the DB-only receipt provider failed.
    #[error("failed to open DB-only receipt provider: {0}")]
    OpenReceiptProvider(String),
    /// Indicates that reading durable receipts failed.
    #[error("failed to read durable receipts for {height}/{hash}: {error}")]
    ReadDurableReceipts {
        /// Block height.
        height: u64,
        /// Block hash.
        hash: B256,
        /// Underlying error text.
        error: String,
    },
    /// Indicates that durable receipts are missing for the block.
    #[error("durable receipts missing for {height}/{hash}")]
    DurableReceiptsMissing {
        /// Block height.
        height: u64,
        /// Block hash.
        hash: B256,
    },
    /// Indicates that a redelivered finalized block is behind the durable marker.
    #[error(
        "redelivered finalized CE block is behind the durable marker: block={height}/{block_hash}, marker={marker:?}"
    )]
    RedeliveredBlockBehindMarker {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Durable marker.
        marker: FinalizedMarker,
    },
    /// Indicates a root conflict between a redelivered block and the durable marker.
    #[error(
        "redelivered finalized CE block root conflicts with the durable marker at {height}/{block_hash}: Reth={authoritative_root}, marker={marker_root}"
    )]
    RedeliveredRootConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Authoritative Reth root.
        authoritative_root: B256,
        /// Marker root.
        marker_root: B256,
    },
    /// Indicates that DB-only state is absent after redelivered persistence notification.
    #[error("Reth emitted persistence for redelivered block {height}/{block_hash}, but DB-only state is absent")]
    RedeliveredDbStateAbsent {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates that the durable parent is absent while validating a redelivered block.
    #[error("durable parent is absent while validating redelivered finalized CE block {height}/{block_hash}")]
    DurableParentAbsentRedelivery {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates that the parent identity of a redelivered block conflicts with the marker.
    #[error(
        "redelivered finalized CE block parent identity conflicts with the durable marker at {height}/{block_hash}: Reth=({reth_hash}, {reth_root}), marker=({marker_hash}, {marker_root})"
    )]
    RedeliveredParentConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Reth parent hash.
        reth_hash: B256,
        /// Reth parent root.
        reth_root: B256,
        /// Marker parent hash.
        marker_hash: B256,
        /// Marker parent root.
        marker_root: B256,
    },
    /// Indicates that DB-only state is absent after persistence notification.
    #[error("Reth emitted persistence for {height}/{block_hash}, but DB-only state is absent")]
    DbStateAbsent {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates that the durable parent is absent for a finalized block.
    #[error("durable parent is absent for finalized CE block {height}/{block_hash}")]
    DurableParentAbsent {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates a conflict between the durable parent header and the finalized CE parent.
    #[error(
        "durable parent/header conflicts with finalized CE parent at {height}/{block_hash}: actor=({actor_hash}, {actor_root}), marker=({marker_hash}, {marker_root})"
    )]
    DurableParentHeaderConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Actor parent hash.
        actor_hash: B256,
        /// Actor parent root.
        actor_root: B256,
        /// Marker parent hash.
        marker_hash: B256,
        /// Marker parent root.
        marker_root: B256,
    },
    /// Indicates that durable canonical CE replay state is missing.
    #[error("durable canonical CE replay missing for finalized block {height}/{block_hash}")]
    DurableCanonicalReplayMissing {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates that a redelivered block conflicts with the durable marker.
    #[error(
        "redelivered finalized CE block conflicts with the durable marker at {height}/{block_hash}: {current:?}"
    )]
    RedeliveredMarkerConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Current durable marker.
        current: FinalizedMarker,
    },
    /// Indicates a parent conflict on the finalized CE candidate.
    #[error(
        "finalized CE candidate parent conflict at {height}/{block_hash}: actor={actor}, candidate={candidate}"
    )]
    CandidateParentConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Actor parent hash.
        actor: B256,
        /// Candidate parent hash.
        candidate: B256,
    },
    /// Indicates a root conflict between durable EVM state and the CE candidate.
    #[error(
        "durable EVM/CE candidate root conflict at {height}/{block_hash}: evm={evm}, candidate={candidate}"
    )]
    CandidateRootConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// EVM root.
        evm: B256,
        /// Candidate root.
        candidate: B256,
    },
    /// Indicates an identity or root conflict during canonical CE replay.
    #[error(
        "durable canonical CE replay identity/root conflict for finalized block {height}/{block_hash}: current={current:?}, replay={replay}, authoritative_root={authoritative_root}"
    )]
    ReplayIdentityRootConflict {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Current marker.
        current: FinalizedMarker,
        /// Formatted replay block.
        replay: String,
        /// Authoritative root.
        authoritative_root: B256,
    },
    /// Indicates that CE MDBX returned a conflicting finalized marker.
    #[error("CE MDBX returned conflicting finalized marker for {height}/{block_hash}: {marker:?}")]
    MdbxConflictingMarker {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Returned marker.
        marker: FinalizedMarker,
    },
    /// Indicates a canonical hash conflict at the finalized height.
    #[error("durable canonical conflict at height {height}: finalized={finalized}, Reth={actual}")]
    DurableCanonicalConflict {
        /// Block height.
        height: u64,
        /// Finalized block hash.
        finalized: B256,
        /// Actual Reth block hash.
        actual: B256,
    },
    /// Indicates that the persistence wait exceeded its deadline.
    #[error(
        "finalized CE persistence deadline exceeded for {height}/{block_hash} after {deadline:?}"
    )]
    PersistenceDeadlineExceeded {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Configured deadline.
        deadline: Duration,
    },
    /// Indicates that the Reth persistence stream closed early.
    #[error("Reth persistence stream ended before finalized CE block {height}/{block_hash}")]
    PersistenceStreamEnded {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
    },
    /// Indicates that Reth persistence advanced past the expected block without matching it.
    #[error(
        "Reth persistence passed finalized CE block {height}/{block_hash} with {notification_number}/{notification_hash}"
    )]
    PersistencePassedBlock {
        /// Block height.
        height: u64,
        /// Block hash.
        block_hash: B256,
        /// Notification block number.
        notification_number: u64,
        /// Notification block hash.
        notification_hash: B256,
    },
}

/// Represents errors from CE replay and recovery verification.
#[derive(Debug, Error)]
pub enum CeRecoveryError {
    /// Indicates a root or parent mismatch when replaying a CE block.
    #[error(
        "replayed CE root/identity mismatch at {number}/{hash}: expected parent {parent_hash}/{parent_root}, root {new_root}; computed {provisional}"
    )]
    ReplayedRootIdentityMismatch {
        /// Block number.
        number: u64,
        /// Block hash.
        hash: B256,
        /// Expected parent hash.
        parent_hash: B256,
        /// Expected parent root.
        parent_root: B256,
        /// Expected new root.
        new_root: B256,
        /// Formatted provisional batch.
        provisional: String,
    },
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
