//! Typed error definitions for DKG handoff, persistence, promotion, and startup.

use alloy_primitives::B256;
use commonware_consensus::types::Epoch;
use thiserror::Error;

/// Represents errors from DKG handoff, persistence, promotion, and startup.
#[derive(Debug, Error)]
pub enum StackDkgError {
    /// Indicates that the recovered pending DKG epoch does not follow the active epoch.
    #[error("recovered pending DKG epoch {pending} does not follow active epoch {active}")]
    PendingEpochDoesNotFollowActive {
        /// Pending DKG epoch.
        pending: Epoch,
        /// Active consensus epoch.
        active: Epoch,
    },

    /// Indicates that the recovered pending DKG epoch missed the activation deadline.
    #[error(
        "recovered pending DKG epoch {pending} missed activation deadline {deadline} at finalized height {finalized_height}"
    )]
    PendingEpochMissedActivationDeadline {
        /// Pending DKG epoch.
        pending: Epoch,
        /// Activation deadline height.
        deadline: u64,
        /// Finalized block height.
        finalized_height: u64,
    },

    /// Indicates that the node failed to read the canonical block hash at a height.
    #[error("failed to read canonical hash at height {height}: {reason}")]
    ReadCanonicalHashAtHeight {
        /// Target block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the finalized preannounce scan found no block hash at a height.
    #[error("canonical finalized preannounce scan is missing block hash at height {height}")]
    MissingPreannounceBlockHash {
        /// Target block height.
        height: u64,
    },

    /// Indicates that the node failed to read a finalized header at a height.
    #[error("failed to read finalized header {height}: {reason}")]
    ReadFinalizedHeader {
        /// Target block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the finalized preannounce scan found no header at a height.
    #[error("canonical finalized preannounce scan is missing header at height {height}")]
    MissingPreannounceHeader {
        /// Target block height.
        height: u64,
    },

    /// Indicates that the finalized preannounce header hash does not match the index hash.
    #[error(
        "canonical finalized preannounce header hash mismatch at height {height}: index {index_hash}, header {header_hash}"
    )]
    PreannounceHeaderHashMismatch {
        /// Target block height.
        height: u64,
        /// Canonical index hash.
        index_hash: B256,
        /// Sealed header hash.
        header_hash: B256,
    },

    /// Indicates that the finalized preannounce header number does not match the height.
    #[error(
        "canonical finalized preannounce header number mismatch: expected {expected}, got {actual}"
    )]
    PreannounceHeaderNumberMismatch {
        /// Expected block number.
        expected: u64,
        /// Actual block number.
        actual: u64,
    },

    /// Indicates that the node failed to decode finalized header artifacts at a height.
    #[error("failed to decode finalized header artifacts at height {height}: {reason}")]
    DecodeFinalizedHeaderArtifacts {
        /// Target block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the validator set has different public key and address counts.
    #[error(
        "validator set has mismatched public key/address lengths: {public_keys} public keys, {addresses} addresses"
    )]
    ValidatorSetPublicKeyAddressLengthMismatch {
        /// Count of public keys.
        public_keys: usize,
        /// Count of addresses.
        addresses: usize,
    },

    /// Indicates that a participant public key is absent from the validator set.
    #[error("participant public key is missing from validator set")]
    ParticipantPublicKeyMissingFromValidatorSet,

    /// Indicates that live join found execution history without a finalized consensus height.
    #[error(
        "startup live join found execution history at height {execution_height} but no durable consensus-finalized height; refusing to recover DKG artifacts from unfinalized execution head. Wait for consensus finalization evidence or use --testnet.trust-el-head for testnet consensus-archive recovery; it cannot bypass the permanent offer-key gate."
    )]
    LiveJoinExecutionHistoryWithoutFinalizedHeight {
        /// Local execution height.
        execution_height: u64,
    },

    /// Indicates that the execution provider does not hold the verified consensus tip.
    #[error("execution provider does not contain the verified consensus tip at height {height}")]
    ExecutionProviderMissingConsensusTip {
        /// Consensus tip height.
        height: u64,
    },

    /// Indicates that a finalized header is absent at the given height.
    #[error("missing finalized header at height {height}")]
    MissingFinalizedHeaderAtHeight {
        /// Target block height.
        height: u64,
    },

    /// Indicates that the node failed to decode header artifacts at a height.
    #[error("failed to decode header artifacts at {height}: {reason}")]
    DecodeHeaderArtifactsAtHeight {
        /// Target block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the node failed to read the block hash at the freeze height.
    #[error("failed to get block hash at freeze height {freeze_height}: {reason}")]
    GetBlockHashAtFreezeHeight {
        /// Freeze block height.
        freeze_height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the node failed to read the canonical header at the freeze height.
    #[error("failed to get canonical header at freeze height {freeze_height}: {reason}")]
    GetCanonicalHeaderAtFreezeHeight {
        /// Freeze block height.
        freeze_height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the freeze header hash does not match the block-hash index.
    #[error(
        "canonical freeze header hash mismatch at height {freeze_height}: block-hash index {block_hash}, header {header_hash}"
    )]
    FreezeHeaderHashMismatch {
        /// Freeze block height.
        freeze_height: u64,
        /// Block-hash index value.
        block_hash: B256,
        /// Sealed header hash.
        header_hash: B256,
    },

    /// Indicates that the freeze header number does not match the freeze height.
    #[error("canonical freeze header number mismatch: expected {expected}, got {actual}")]
    FreezeHeaderNumberMismatch {
        /// Expected block number.
        expected: u64,
        /// Actual block number.
        actual: u64,
    },

    /// Indicates that the node failed to read state at the freeze height.
    #[error("failed to get state at freeze height {freeze_height}: {reason}")]
    GetStateAtFreezeHeight {
        /// Freeze block height.
        freeze_height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the refreshed participant set is not valid.
    #[error("invalid participant set after refresh: {reason}")]
    InvalidParticipantSetAfterRefresh {
        /// Error description.
        reason: String,
    },

    /// Indicates that DKG participant recovery requires a keys directory.
    #[error("DKG participant recovery requires --consensus.keys-dir")]
    DkgParticipantRecoveryRequiresKeysDir,

    /// Indicates that the decoded DKG boundary outcome epoch does not match the artifact epoch.
    #[error(
        "DKG boundary outcome epoch {decoded_epoch} does not match artifact epoch {artifact_epoch}"
    )]
    BoundaryOutcomeEpochMismatch {
        /// Decoded outcome epoch.
        decoded_epoch: u64,
        /// Artifact epoch.
        artifact_epoch: u64,
    },

    /// Indicates that the node failed to encode a pending DKG boundary artifact.
    #[error("failed to encode pending DKG boundary artifact: {reason}")]
    EncodePendingBoundaryArtifact {
        /// Error description.
        reason: String,
    },

    /// Indicates that the encoded pending DKG boundary artifact exceeds the size limit.
    #[error("pending DKG boundary artifact too large: {len} bytes")]
    PendingBoundaryArtifactTooLarge {
        /// Encoded byte length.
        len: usize,
    },

    /// Indicates that the pending DKG boundary snapshot is shorter than the header size.
    #[error("pending DKG boundary snapshot too short: {actual} < {expected}")]
    PendingBoundarySnapshotTooShort {
        /// Actual byte length.
        actual: usize,
        /// Minimum required byte length.
        expected: usize,
    },

    /// Indicates that the pending DKG boundary snapshot uses legacy version ODKGPB01.
    #[error("unsupported pending DKG boundary snapshot version ODKGPB01")]
    UnsupportedPendingBoundarySnapshotVersion,

    /// Indicates that the pending DKG boundary snapshot has invalid magic bytes.
    #[error("invalid pending DKG boundary snapshot magic")]
    InvalidPendingBoundarySnapshotMagic,

    /// Indicates that the pending DKG boundary snapshot has an invalid height field.
    #[error("invalid pending DKG boundary height field")]
    InvalidPendingBoundaryHeightField,

    /// Indicates that the pending DKG boundary snapshot has an invalid length field.
    #[error("invalid pending DKG boundary length field")]
    InvalidPendingBoundaryLengthField,

    /// Indicates that the pending DKG boundary length calculation overflowed.
    #[error("pending DKG boundary length overflow")]
    PendingBoundaryLengthOverflow,

    /// Indicates that the pending DKG boundary snapshot length does not match the header.
    #[error("pending DKG boundary snapshot length mismatch: expected {expected}, got {actual}")]
    PendingBoundarySnapshotLengthMismatch {
        /// Expected byte length.
        expected: usize,
        /// Actual byte length.
        actual: usize,
    },

    /// Indicates that the node failed to decode a pending DKG boundary artifact.
    #[error("failed to decode pending DKG boundary artifact: {reason}")]
    DecodePendingBoundaryArtifact {
        /// Error description.
        reason: String,
    },

    /// Indicates that the pending DKG boundary snapshot does not hold a boundary outcome.
    #[error("pending DKG boundary snapshot does not contain a BoundaryOutcome")]
    PendingBoundarySnapshotMissingBoundaryOutcome,

    /// Indicates that the completed DKG participant set does not match output players.
    #[error("completed DKG participant set does not match reconstructed output players")]
    CompletedParticipantSetMismatch,

    /// Indicates that freeze-height state is unavailable for pending boundary validation.
    #[error(
        "pending DKG boundary freeze-height state unavailable at height {height}; refusing unsafe recovery"
    )]
    PendingBoundaryFreezeHeightStateUnavailable {
        /// Freeze block height.
        height: u64,
    },

    /// Indicates that the pending boundary snapshot does not match the freeze-height state.
    #[error(
        "pending DKG boundary snapshot does not match freeze-height validator set and DKG output"
    )]
    PendingBoundarySnapshotFreezeMismatch,

    /// Indicates that the finalized DKG boundary conflicts with the pending snapshot.
    #[error("finalized DKG boundary conflicts with pending snapshot for cycle {cycle}")]
    FinalizedBoundaryConflictsWithPendingSnapshot {
        /// DKG cycle number.
        cycle: u64,
    },

    /// Indicates that the finalized pending boundary includes the node without private material.
    #[error(
        "finalized pending DKG boundary includes the local validator but pending private material is unavailable"
    )]
    FinalizedPendingBoundaryMissingPrivateMaterial,

    /// Indicates that the pending boundary includes the node without matching private material.
    #[error(
        "pending DKG boundary includes the local validator but matching pending private material is unavailable"
    )]
    PendingBoundaryMissingMatchingPrivateMaterial,

    /// Indicates that freeze-height state is unavailable during pending DKG runtime restore.
    #[error(
        "pending DKG freeze-height state unavailable at height {height} during runtime restore"
    )]
    PendingFreezeHeightStateUnavailableDuringRestore {
        /// Freeze block height.
        height: u64,
    },

    /// Indicates that pending DKG TEE expiry exclusions do not match freeze-height state.
    #[error("pending DKG TEE expiry exclusions do not match freeze-height state")]
    PendingTeeExpiryExclusionsMismatch,

    /// Indicates that the pending boundary includes the node without private material.
    #[error(
        "pending DKG boundary includes the local validator but pending private material is unavailable"
    )]
    PendingBoundaryMissingPrivateMaterial,

    /// Indicates that a DKG state directory is missing one or more required files.
    #[error(
        "{label} DKG state is incomplete in {dir}: expected {share_file}, {polynomial_file}, and {output_file}"
    )]
    IncompleteDkgState {
        /// State category label.
        label: &'static str,
        /// Directory path string.
        dir: String,
        /// Share file name.
        share_file: &'static str,
        /// Polynomial file name.
        polynomial_file: &'static str,
        /// Output file name.
        output_file: &'static str,
    },

    /// Indicates that the finalized boundary output does not match active local DKG output.
    #[error("finalized DKG boundary output does not match active local DKG output")]
    FinalizedBoundaryOutputMismatch,

    /// Indicates that the node failed to read the finalized boundary carrier header.
    #[error("failed to read finalized boundary carrier {height}: {reason}")]
    ReadFinalizedBoundaryCarrier {
        /// Carrier block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the node failed to decode finalized boundary carrier artifacts.
    #[error("failed to decode finalized boundary carrier artifacts at {height}: {reason}")]
    DecodeFinalizedBoundaryCarrierArtifacts {
        /// Carrier block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that saved DKG material does not match the finalized VRF group public key.
    #[error("saved DKG material does not match finalized VRF group public key")]
    SavedDkgMaterialVrfGroupKeyMismatch,

    /// Indicates that the validator set drifted from the saved DKG material.
    #[error(
        "validator set has drifted from saved DKG: recovered DKG output has {recovered} player(s) but the recovered boundary (epoch {epoch}, activation height {activation_height}) recorded an active set of {recorded} validator(s) - the restored consensus material does not match the chain's recovered DKG boundary"
    )]
    ValidatorSetDriftedFromSavedDkg {
        /// Recovered player count.
        recovered: usize,
        /// Boundary epoch.
        epoch: u64,
        /// Planned activation height.
        activation_height: u64,
        /// Recorded validator count.
        recorded: usize,
    },

    /// Indicates that the node failed to read a header at a height.
    #[error("failed to read header {height}: {reason}")]
    ReadHeaderAtHeight {
        /// Target block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the node failed to read the last block number.
    #[error("failed to get last block number: {reason}")]
    GetLastBlockNumber {
        /// Error description.
        reason: String,
    },

    /// Indicates that the node failed to read the block hash for a height.
    #[error("failed to get block hash for height {height}: {reason}")]
    GetBlockHashForHeight {
        /// Target block height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the block hash is absent for an execution height.
    #[error("missing block hash for execution height {height}; refusing genesis fallback")]
    MissingBlockHashForExecutionHeight {
        /// Execution block height.
        height: u64,
    },

    /// Indicates that the participant set is not valid.
    #[error("invalid participant set: {reason}")]
    InvalidParticipantSet {
        /// Error description.
        reason: String,
    },

    /// Indicates that the genesis formation probe timed out before DKG round 0.
    #[error(
        "could not prove genesis formation before DKG round 0: connected_reth_peers={connected_peers} required_remote_peers={required_remote_peers} configured_remote_peers={expected_remote_peers} reth_syncing={is_syncing} reth_initial_syncing={is_initially_syncing} peer_query_failed={peer_query_failed}"
    )]
    GenesisFormationProbeTimeout {
        /// Connected Reth peer count.
        connected_peers: usize,
        /// Required remote peer count.
        required_remote_peers: usize,
        /// Configured remote peer count.
        expected_remote_peers: usize,
        /// Whether Reth is syncing.
        is_syncing: bool,
        /// Whether Reth is initially syncing.
        is_initially_syncing: bool,
        /// Whether the peer query failed.
        peer_query_failed: bool,
    },

    /// Indicates that startup cannot recover threshold material before sync starts.
    #[error(
        "startup cannot recover threshold material before sync starts: {reason}. Restart with the current --consensus.public-polynomial and --consensus.dkg-output without --consensus.signing-share so the node can sync as VerifierOnly and acquire a share through the running reshare path"
    )]
    MissingCurrentThresholdMaterial {
        /// Failure reason.
        reason: String,
    },

    /// Indicates that a fresh genesis founder found a pre-existing canonical offer key.
    #[error("fresh block-1 founding startup found a pre-existing canonical offer key")]
    FreshGenesisFoundPreexistingOfferKey,

    /// Indicates that an existing identity has no resident permanent offer key.
    #[error(
        "existing identity has no permanent resident offer key before threshold work; no recovery or fallback exists"
    )]
    ExistingIdentityMissingResidentOfferKey,

    /// Indicates that an existing identity has a zero resident permanent offer key.
    #[error(
        "existing identity has a zero permanent resident offer key before threshold work; no recovery or fallback exists"
    )]
    ExistingIdentityZeroResidentOfferKey,

    /// Indicates that existing canonical state has no mandatory OST3 offer key.
    #[error(
        "existing canonical state has no mandatory OST3 offer key; no recovery or fallback exists"
    )]
    ExistingCanonicalStateMissingOfferKey,

    /// Indicates that a non-verifier attempted to defer offer-key comparison.
    #[error(
        "only an empty-DB verifier join with an installed permanent key may defer exact offer-key comparison until certified sync"
    )]
    NonVerifierDeferredOfferKeyComparison,

    /// Indicates that the local enclave offer key does not match canonical state.
    #[error(
        "local enclave does not hold the canonical permanent offer key before threshold work; no recovery or fallback exists"
    )]
    ResidentOfferKeyMismatch,

    /// Indicates that recovered DKG boundary players do not match active participants.
    #[error("recovered DKG boundary output players do not match active participant set")]
    RecoveredBoundaryPlayersMismatch,

    /// Indicates that recovered DKG boundary active-set length does not match participant count.
    #[error(
        "recovered DKG boundary active-set length {addresses_len} does not match participant count {participants_len}"
    )]
    RecoveredBoundaryActiveSetLengthMismatch {
        /// Active-set address count.
        addresses_len: usize,
        /// Participant count.
        participants_len: usize,
    },

    /// Indicates that recovered DKG boundary active-set hash does not match addresses.
    #[error("recovered DKG boundary active-set hash does not match active-set addresses")]
    RecoveredBoundaryActiveSetHashMismatch,

    /// Indicates that recovered DKG boundary VRF key bytes do not match the key hash.
    #[error("recovered DKG boundary VRF group public key bytes do not match hash")]
    RecoveredBoundaryVrfKeyBytesMismatch,

    /// Indicates that an encoded consensus public key has an unexpected byte length.
    #[error("encoded MinPk consensus pubkey has unexpected length: expected 48, got {actual}")]
    EncodedConsensusPubkeyLengthMismatch {
        /// Actual byte length.
        actual: usize,
    },

    /// Indicates that recovered DKG boundary committee hash does not match the committee snapshot.
    #[error(
        "recovered DKG boundary committee_set_hash does not match boundary committee/address mapping"
    )]
    RecoveredBoundaryCommitteeSetHashMismatch,

    /// Indicates that TEE bootstrap requires a validator EVM key.
    #[error("TEE bootstrap requires a validator EVM key")]
    TeeBootstrapRequiresEvmKey,

    /// Indicates that the node failed to load the validator EVM signer for TEE bootstrap.
    #[error("failed to load validator EVM signer for TEE bootstrap: {reason}")]
    LoadValidatorEvmSignerForTeeBootstrap {
        /// Error description.
        reason: String,
    },

    /// Indicates that a DKG output player is absent from the frozen validator set.
    #[error("DKG output contains a player absent from the frozen validator set")]
    DkgOutputPlayerAbsentFromFrozenValidatorSet,

    /// Indicates that the DKG output participant set is not valid.
    #[error("invalid DKG output participant set: {reason}")]
    InvalidDkgOutputParticipantSet {
        /// Error description.
        reason: String,
    },

    /// Indicates that the DKG output player count does not match the validator set size.
    #[error(
        "DKG output player count {players_len} does not match validator set size {validators_len}"
    )]
    DkgOutputPlayerCountMismatch {
        /// DKG output player count.
        players_len: usize,
        /// Validator set size.
        validators_len: usize,
    },

    /// Indicates that a validator public key is absent from the DKG output players.
    #[error("validator set public key is missing from DKG output players")]
    ValidatorPublicKeyMissingFromDkgOutputPlayers,

    /// Indicates that a pending DKG boundary snapshot lacks matching DKG material.
    #[error(
        "pending DKG boundary snapshot was recovered but matching DKG material is unavailable"
    )]
    PendingBoundarySnapshotMissingMaterial,
}
