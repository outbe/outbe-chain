//! Typed error definitions for epoch activation, supervision, recovery, and transport.

use alloy_primitives::{Address, B256};
use commonware_consensus::types::Epoch;
use outbe_consensus::digest::Digest;
use std::time::Duration;
use thiserror::Error;

/// Represents errors from epoch activation, supervision, recovery, and transport.
#[derive(Debug, Error)]
pub enum StackEpochError {
    /// Indicates that pending DKG activation missed VRF expiry.
    #[error(
        "pending DKG activation missed VRF expiry: cycle {cycle}, height {height}, deadline {deadline}"
    )]
    PendingActivationMissedVrfExpiry {
        /// DKG cycle number.
        cycle: u64,
        /// Finalized block height.
        height: u64,
        /// Expiry deadline height.
        deadline: u64,
    },

    /// Indicates that pending DKG activation was absent after the precheck.
    #[error("pending DKG activation missing after precheck at height {height}")]
    PendingActivationMissingAfterPrecheck {
        /// Activation block height.
        height: u64,
    },

    /// Indicates that the completed DKG boundary epoch does not match the activation epoch.
    #[error(
        "completed DKG boundary epoch {boundary_epoch} does not match activation epoch {activation_epoch}"
    )]
    CompletedBoundaryEpochMismatch {
        /// Boundary artifact epoch.
        boundary_epoch: u64,
        /// Expected activation epoch.
        activation_epoch: u64,
    },

    /// Indicates that the completed DKG boundary is absent from the manager at activation.
    #[error("completed DKG boundary is missing from manager at activation")]
    CompletedBoundaryMissingFromManager,

    /// Indicates that the published preannounce boundary diverged before activation.
    #[error("published pre-announce boundary diverged before activation")]
    PublishedPreannounceBoundaryDiverged,

    /// Indicates that dealer-only activation disappeared while preparing the boundary.
    #[error("dealer-only activation disappeared while preparing boundary")]
    DealerOnlyActivationDisappearedPreparingBoundary,

    /// Indicates that dealer-only activation disappeared before boundary publication.
    #[error("dealer-only activation disappeared before boundary publication")]
    DealerOnlyActivationDisappearedBeforePublication,

    /// Indicates that dealer-only DKG activation missed VRF expiry.
    #[error(
        "dealer-only DKG activation missed VRF expiry: cycle {cycle}, height {height}, deadline {deadline}"
    )]
    DealerOnlyActivationMissedVrfExpiry {
        /// DKG cycle number.
        cycle: u64,
        /// Finalized block height.
        height: u64,
        /// Expiry deadline height.
        deadline: u64,
    },

    /// Indicates that a dealer-only pending boundary lost its canonical output before activation.
    #[error("dealer-only pending boundary lost its canonical output before activation")]
    DealerOnlyPendingBoundaryLostCanonicalOutput,

    /// Indicates that a dealer-only completed boundary is absent from the manager at activation.
    #[error("dealer-only completed DKG boundary is missing from manager at activation")]
    DealerOnlyBoundaryMissingFromManager,

    /// Indicates that the dealer-only published preannounce boundary diverged before activation.
    #[error("dealer-only published preannounce boundary diverged before activation")]
    DealerOnlyPublishedPreannounceDiverged,

    /// Indicates that dealer-only activation was absent after the handoff decision.
    #[error("dealer-only activation missing after decision at height {height}")]
    DealerOnlyActivationMissingAfterDecision {
        /// Activation block height.
        height: u64,
    },

    /// Indicates that the node failed to read the latest block height after DKG completion.
    #[error("failed to read latest block height after DKG completion: {reason}")]
    ReadLatestBlockHeightAfterCompletion {
        /// Error description.
        reason: String,
    },

    /// Indicates that the dealer-only DKG participant set does not match the frozen target.
    #[error("dealer-only DKG participant set does not match frozen target")]
    DealerOnlyParticipantSetMismatch,

    /// Indicates that the node failed to read the latest block height after dealer-only DKG.
    #[error("failed to read latest block height after dealer-only DKG completion: {reason}")]
    ReadLatestBlockHeightAfterDealerOnlyCompletion {
        /// Error description.
        reason: String,
    },

    /// Indicates that an epoch restart timed out waiting for a finalized anchor.
    #[error(
        "epoch={epoch} restart without finalized anchor after {timeout:?}; handle_genesis would return ZERO, or Phase 1 would lack the finalized-round proof key for parent_view=0"
    )]
    EpochRestartWithoutFinalizedAnchor {
        /// Target consensus epoch.
        epoch: u64,
        /// Wait timeout duration.
        timeout: Duration,
    },

    /// Indicates that DKG activation timed out waiting for the finalization actor.
    #[error(
        "DKG activation race after {timeout:?}: finalized_anchor=(height={finalized}, hash={finalized_hash}, round_ready={round_ready}) activation_height={activation_height}; FinalizationActor is lagging the DKG manager"
    )]
    DkgActivationRaceTimeout {
        /// Wait timeout duration.
        timeout: Duration,
        /// Last finalized block height.
        finalized: u64,
        /// Last finalized block hash.
        finalized_hash: B256,
        /// Whether the finalized round is ready.
        round_ready: bool,
        /// Target activation height.
        activation_height: u64,
    },

    /// Indicates that dealer demotion activation timed out waiting for finalization.
    #[error(
        "exited-validator demotion activation race after {timeout:?}: finalized=(height={finalized}, hash={finalized_hash}, round_ready={round_ready}) activation_height={activation_height}"
    )]
    DealerDemotionActivationRaceTimeout {
        /// Wait timeout duration.
        timeout: Duration,
        /// Last finalized block height.
        finalized: u64,
        /// Last finalized block hash.
        finalized_hash: B256,
        /// Whether the finalized round is ready.
        round_ready: bool,
        /// Target activation height.
        activation_height: u64,
    },

    /// Indicates that the execution watchdog failed to read the provider head.
    #[error("execution watchdog provider read failed for {unhealthy_for:?}: {reason}")]
    WatchdogProviderReadFailed {
        /// Unhealthy duration.
        unhealthy_for: Duration,
        /// Error description.
        reason: String,
    },

    /// Indicates that the execution watchdog failed to read the provider hash at consensus tip.
    #[error(
        "execution watchdog provider hash read failed at consensus tip height {height} for {unhealthy_for:?}: {reason}"
    )]
    WatchdogProviderHashReadFailed {
        /// Consensus tip height.
        height: u64,
        /// Unhealthy duration.
        unhealthy_for: Duration,
        /// Error description.
        reason: String,
    },

    /// Indicates that the Reth provider head or hash remained behind the consensus tip.
    #[error(
        "execution watchdog fatal: Reth provider head/hash not ready for consensus tip height {consensus_tip_height} digest {digest} (reth_head={reth_head_height}, provider_tip_hash={provider_tip_hash:?}, unhealthy_for={unhealthy_for:?})"
    )]
    WatchdogFatalProviderNotReady {
        /// Consensus tip height.
        consensus_tip_height: u64,
        /// Consensus tip digest.
        digest: Digest,
        /// Reth head height.
        reth_head_height: u64,
        /// Provider block hash at consensus tip height.
        provider_tip_hash: Option<B256>,
        /// Unhealthy duration.
        unhealthy_for: Duration,
    },

    /// Indicates that a frozen DKG target expired before activation.
    #[error("{diagnostic}: cycle {cycle}, height {height}, deadline {deadline}")]
    DkgTargetExpired {
        /// Diagnostic prefix.
        diagnostic: &'static str,
        /// DKG cycle number.
        cycle: u64,
        /// Finalized block height.
        height: u64,
        /// Expiry deadline height.
        deadline: u64,
    },

    /// Indicates that the view retention timeout calculation overflowed.
    #[error("view retention timeout overflow")]
    ViewRetentionTimeoutOverflow,

    /// Indicates that startup recovered a pending DKG boundary without a keys directory.
    #[error("recovered pending DKG boundary without configured keys directory")]
    RecoveredPendingBoundaryWithoutKeysDir,

    /// Indicates that a recovered dealer-only DKG handoff has no boundary artifact.
    #[error("recovered dealer-only DKG handoff has no boundary artifact")]
    RecoveredDealerOnlyHandoffMissingBoundaryArtifact,

    /// Indicates that a deferred startup DKG plan changed the active epoch.
    #[error("deferred startup DKG plan changed active epoch")]
    DeferredStartupPlanChangedActiveEpoch,

    /// Indicates that a recovered dealer-only DKG activation has no boundary artifact.
    #[error("recovered dealer-only DKG activation has no boundary artifact")]
    RecoveredDealerOnlyActivationMissingBoundaryArtifact,

    /// Indicates that the recovered DKG boundary epoch does not match the startup epoch.
    #[error(
        "recovered DKG boundary epoch {boundary_epoch} does not match activated startup epoch {active_epoch}"
    )]
    RecoveredBoundaryEpochMismatchWithStartup {
        /// Boundary artifact epoch.
        boundary_epoch: u64,
        /// Activated startup epoch.
        active_epoch: Epoch,
    },

    /// Indicates that the recovered DKG VRF material version does not follow the active version.
    #[error(
        "recovered DKG VRF material version {boundary_version} does not follow active version {active_version}"
    )]
    RecoveredVrfVersionMismatch {
        /// Boundary VRF material version.
        boundary_version: u64,
        /// Active VRF material version.
        active_version: u64,
    },

    /// Indicates that the peer manager closed during recovery admission.
    #[error("peer_manager closed during recovery admission")]
    PeerManagerClosedDuringRecoveryAdmission,

    /// Indicates that the follower pre-stop handshake is not installed.
    #[error("follower pre-stop handshake is not installed")]
    FollowerPreStopHandshakeNotInstalled,

    /// Indicates that the consensus signing key argument is missing.
    #[error("--consensus.signing-key is required")]
    SigningKeyRequired,

    /// Indicates that the startup P2P admission block hash is missing.
    #[error("startup P2P admission block is missing")]
    StartupP2pAdmissionBlockMissing,

    /// Indicates that the node failed to read the recovery-anchor block hash.
    #[error("failed to read recovery-anchor block hash at height {height}: {reason}")]
    ReadRecoveryAnchorBlockHash {
        /// Recovery anchor height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the canonical block hash is absent for the recovery anchor.
    #[error("missing canonical block hash for recovery anchor at height {height}")]
    MissingRecoveryAnchorBlockHash {
        /// Recovery anchor height.
        height: u64,
    },

    /// Indicates that fresh bootstrap lost its canonical genesis DKG boundary.
    #[error("fresh bootstrap lost its canonical genesis DKG boundary")]
    FreshBootstrapLostGenesisBoundary,

    /// Indicates that the genesis DKG boundary VRF version differs from the active version.
    #[error("genesis DKG boundary VRF material version differs from the active startup version")]
    GenesisBoundaryVrfVersionDiffersFromActive,

    /// Indicates that the consensus storage directory is not set before stack startup.
    #[error("consensus storage_dir must be set before stack startup")]
    ConsensusStorageDirRequired,

    /// Indicates that the Radicle voting gate is in a fatal state.
    #[error("Radicle voting gate is fatal: {reason}")]
    RadicleVotingGateFatal {
        /// Fatal gate reason.
        reason: &'static str,
    },

    /// Indicates that the node failed to build the verifier scheme for an epoch.
    #[error("failed to build verifier scheme for validator set (epoch {epoch})")]
    BuildVerifierSchemeFailed {
        /// Target consensus epoch.
        epoch: Epoch,
    },

    /// Indicates that the current reshare target lacks a BLS public key for an EVM address.
    #[error("current reshare target is missing the BLS public key for EVM address {address}")]
    ReshareTargetMissingBlsPublicKey {
        /// Validator EVM address.
        address: Address,
    },

    /// Indicates that the validator EVM key belongs to a different BLS key in the reshare target.
    #[error(
        "validator EVM key address {address} belongs to a different BLS consensus key in the current reshare target"
    )]
    EvmKeyBelongsToDifferentReshareBlsKey {
        /// Validator EVM address.
        address: Address,
    },

    /// Indicates that the local BLS key is absent from the recovered boundary committee.
    #[error(
        "local BLS key is not in recovered DKG boundary committee for epoch {epoch}; refusing latest-state EVM signer authorization"
    )]
    LocalBlsKeyNotInRecoveredBoundaryCommittee {
        /// Boundary epoch.
        epoch: u64,
    },

    /// Indicates that the recovered boundary address mapping lacks a participant index.
    #[error("recovered DKG boundary address mapping missing participant index {index}")]
    RecoveredBoundaryAddressMappingMissingParticipant {
        /// Participant index.
        index: usize,
    },

    /// Indicates that the validator EVM address does not match the recovered boundary address.
    #[error(
        "validator EVM key address {signer_address} does not match recovered DKG boundary address {expected_address} for local BLS consensus key"
    )]
    EvmKeyMismatchWithRecoveredBoundaryAddress {
        /// Local signer EVM address.
        signer_address: Address,
        /// Expected boundary EVM address.
        expected_address: Address,
    },

    /// Indicates that the validator EVM address is absent from active consensus and validator sets.
    #[error(
        "validator EVM key address {address} is neither in the active consensus participant set nor the active validator set"
    )]
    EvmKeyNotInActiveSets {
        /// Local signer EVM address.
        address: Address,
    },

    /// Indicates that the validator set lacks a BLS public key for an EVM address.
    #[error("validator set missing BLS public key for EVM address {address}")]
    ValidatorSetMissingBlsPublicKey {
        /// Local signer EVM address.
        address: Address,
    },

    /// Indicates that the validator EVM address belongs to a different BLS consensus key.
    #[error("validator EVM key address {address} belongs to a different BLS consensus key")]
    EvmKeyBelongsToDifferentBlsKey {
        /// Local signer EVM address.
        address: Address,
    },

    /// Indicates that the signing key or BLS share is invalid for the validator set.
    #[error("signing key or BLS share invalid for validator set (epoch {epoch})")]
    InvalidSignerSchemeForEpoch {
        /// Target consensus epoch.
        epoch: Epoch,
    },

    /// Indicates that the verifier scheme is invalid for the validator set.
    #[error(
        "verifier scheme invalid for validator set (epoch {epoch}): polynomial total ({polynomial_total}) must equal participant count ({participant_count})"
    )]
    InvalidVerifierSchemeForEpoch {
        /// Target consensus epoch.
        epoch: Epoch,
        /// Active polynomial total count.
        polynomial_total: u32,
        /// Active participant count.
        participant_count: usize,
    },

    /// Indicates that a recovered future DKG handoff collided with an existing subchannel stash.
    #[error("recovered future DKG handoff collided with an existing subchannel stash")]
    RecoveredFutureDkgHandoffSubchannelCollision,

    /// Indicates that the executor actor task failed.
    #[error("executor actor task failed: {reason}")]
    ExecutorActorTaskFailed {
        /// Task failure description.
        reason: String,
    },

    /// Indicates that the application handler task failed.
    #[error("application handler task failed: {reason}")]
    ApplicationHandlerTaskFailed {
        /// Task failure description.
        reason: String,
    },

    /// Indicates that the finalization actor task failed.
    #[error("finalization actor task failed: {reason}")]
    FinalizationActorTaskFailed {
        /// Task failure description.
        reason: String,
    },

    /// Indicates that the peer manager actor exited unexpectedly.
    #[error("peer manager actor exited: {reason}")]
    PeerManagerActorExited {
        /// Exit error description.
        reason: String,
    },

    /// Indicates that the marshal actor exited unexpectedly.
    #[error("marshal actor exited: {reason}")]
    MarshalActorExited {
        /// Exit error description.
        reason: String,
    },

    /// Indicates that a mandatory TEE chain requires the enclave socket argument.
    #[error("mandatory TEE chain requires --tee-enclave-socket before consensus startup")]
    MandatoryTeeRequiresEnclaveSocket,

    /// Indicates that the mandatory teeAttestationV1 ChainSpec configuration is invalid.
    #[error("invalid mandatory teeAttestationV1 ChainSpec: {reason}")]
    InvalidTeeAttestationChainSpec {
        /// Error description.
        reason: String,
    },

    /// Indicates a TEE policy or session resolution error.
    #[error("{reason}")]
    TeePolicyError {
        /// Error description.
        reason: String,
    },

    /// Indicates that a founding validator TEE bootstrap lacks its EVM identity.
    #[error("founding validator TEE bootstrap requires its EVM identity")]
    FoundingValidatorTeeBootstrapRequiresEvmIdentity,

    /// Indicates that the TEE DKG P2P channel was not registered.
    #[error("TEE DKG P2P channel not registered")]
    TeeDkgChannelNotRegistered,

    /// Indicates that the TEE bootstrap P2P channel was not registered.
    #[error("TEE bootstrap P2P channel not registered")]
    TeeBootstrapChannelNotRegistered,

    /// Indicates that fresh OST3 bootstrap lacks its canonical genesis DKG boundary.
    #[error("fresh OST3 bootstrap is missing its canonical genesis DKG boundary")]
    FreshOst3BootstrapMissingGenesisBoundary,

    /// Indicates that the OST3 epoch-0 DKG boundary differs from live genesis participants.
    #[error("OST3 epoch-0 DKG boundary differs from the live genesis participants")]
    Ost3Epoch0BoundaryDiffersFromLiveParticipants,

    /// Indicates that the local validator is absent from the OST3 epoch-0 committee snapshot.
    #[error("local validator is absent from the exact OST3 epoch-0 committee snapshot")]
    LocalValidatorAbsentFromOst3Epoch0Committee,

    /// Indicates that the OST3 deterministic block-1 lease overflows u64.
    #[error("OST3 deterministic block-1 lease overflows u64")]
    Ost3Block1LeaseOverflow,

    /// Indicates that the TEE enclave endpoint path is not valid UTF-8.
    #[error("TEE enclave endpoint is not valid UTF-8")]
    TeeEnclaveEndpointInvalidUtf8,

    /// Indicates that the Reth P2P signing key is invalid.
    #[error("invalid Reth P2P signing key: {reason}")]
    InvalidRethP2pSigningKey {
        /// Error description.
        reason: String,
    },

    /// Indicates that the Reth P2P public key is not compressed SEC1-33.
    #[error("Reth P2P public key is not compressed SEC1-33")]
    RethP2pPublicKeyNotCompressedSec1,

    /// Indicates that reconnecting to the production NodeHost enclave failed.
    #[error("production NodeHost reconnect failed: {reason}")]
    ProductionNodeHostReconnectFailed {
        /// Error description.
        reason: String,
    },

    /// Indicates that loading the committed NodeHost manifest failed.
    #[error("committed NodeHost manifest load failed: {reason}")]
    CommittedNodeHostManifestLoadFailed {
        /// Error description.
        reason: String,
    },

    /// Indicates that reconnecting to the GramineDirectDev enclave failed.
    #[error("GramineDirectDev enclave reconnect failed: {reason}")]
    GramineDirectDevReconnectFailed {
        /// Error description.
        reason: String,
    },

    /// Indicates that the startup TEE DKG ceremony failed.
    #[error("TEE DKG ceremony failed: {reason}")]
    TeeDkgCeremonyFailed {
        /// Error description.
        reason: String,
    },

    /// Indicates that the startup TEE bootstrap coordination failed.
    #[error("TEE bootstrap coordination failed: {reason}")]
    TeeBootstrapCoordinationFailed {
        /// Error description.
        reason: String,
    },

    /// Indicates that TEE DKG and bootstrap timed out before block 1.
    #[error(
        "TEE DKG + bootstrap did not complete within {timeout_secs}s (--tee-bootstrap-timeout-secs); halting before block 1"
    )]
    TeeBootstrapTimeout {
        /// Timeout in seconds.
        timeout_secs: u64,
    },

    /// Indicates that a verifier-join enclave lacks a permanent resident offer key.
    #[error(
        "verifier-join enclave has no permanent resident offer key; refusing certified sync (no recovery or fallback)"
    )]
    VerifierJoinEnclaveMissingResidentOfferKey,

    /// Indicates that an active validator consensus startup lacks its EVM identity.
    #[error("active validator consensus startup requires its EVM identity")]
    ActiveValidatorStartupRequiresEvmIdentity,

    /// Indicates that a mandatory OST3 chain has no on-chain offer key after block 1.
    #[error("mandatory OST3 chain has no on-chain offer key after block 1")]
    MandatoryOst3MissingOnChainOfferKeyAfterBlock1,

    /// Indicates that the local enclave does not hold the chain offer key.
    #[error(
        "local enclave does not hold the chain offer key; refusing consensus startup (no recovery or fallback)"
    )]
    LocalEnclaveDoesNotHoldChainOfferKey,

    /// Indicates that registering the initial DKG subchannel failed.
    #[error("failed to register initial DKG subchannel: {reason}")]
    RegisterInitialDkgSubchannel {
        /// Error description.
        reason: String,
    },

    /// Indicates that fresh bootstrap lacks the full DKG output artifact.
    #[error(
        "fresh bootstrap requires full DKG output; public polynomial alone cannot build canonical boundary"
    )]
    FreshBootstrapRequiresFullDkgOutput,

    /// Indicates that the participant set is not valid.
    #[error("invalid participant set: {reason}")]
    InvalidParticipantSet {
        /// Error description.
        reason: String,
    },

    /// Indicates that the P2P oracle closed during startup admission.
    #[error("P2P oracle closed during startup admission")]
    P2pOracleClosedDuringStartupAdmission,

    /// Indicates that pre-registering TEE DKG round 0 failed.
    #[error("failed to pre-register TEE DKG round 0: {reason}")]
    PreRegisterTeeDkgRound0 {
        /// Error description.
        reason: String,
    },

    /// Indicates that pre-registering TEE bootstrap round 0 failed.
    #[error("failed to pre-register TEE bootstrap round 0: {reason}")]
    PreRegisterTeeBootstrapRound0 {
        /// Error description.
        reason: String,
    },

    /// Indicates that reading the provider block hash at the consensus tip height failed.
    #[error("failed to read provider block hash at consensus tip height {height}: {reason}")]
    ReadProviderBlockHashAtConsensusTip {
        /// Consensus tip height.
        height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that the local validator is neither a previous dealer nor a target player.
    #[error(
        "local validator is neither previous DKG dealer nor frozen target player at height {height}"
    )]
    LocalValidatorNotParticipantAtHeight {
        /// Freeze block height.
        height: u64,
    },

    /// Indicates that the frozen validator set block hash was unavailable by planned activation.
    #[error(
        "frozen validator set block hash unavailable by planned activation: freeze height {freeze_height}, current height {current_height}, planned activation {planned_activation_height}"
    )]
    FrozenValidatorSetBlockHashUnavailable {
        /// Freeze block height.
        freeze_height: u64,
        /// Current block height.
        current_height: u64,
        /// Planned activation height.
        planned_activation_height: u64,
    },

    /// Indicates that refreshing the frozen validator set at the freeze height failed.
    #[error("failed to refresh frozen validator set at height {freeze_height}: {reason}")]
    RefreshFrozenValidatorSetFailed {
        /// Freeze block height.
        freeze_height: u64,
        /// Error description.
        reason: String,
    },

    /// Indicates that a DKG ceremony precondition failed.
    #[error("{message}")]
    CeremonyPreconditionFailed {
        /// Static failure description.
        message: &'static str,
    },
}

impl StackEpochError {
    /// Creates an error when the executor actor task fails.
    pub fn executor_actor_task_failed(error: impl std::fmt::Debug) -> Self {
        Self::ExecutorActorTaskFailed {
            reason: format!("{error:?}"),
        }
    }

    /// Creates an error when the application handler task fails.
    pub fn application_handler_task_failed(error: impl std::fmt::Debug) -> Self {
        Self::ApplicationHandlerTaskFailed {
            reason: format!("{error:?}"),
        }
    }

    /// Creates an error when the finalization actor task fails.
    pub fn finalization_actor_task_failed(error: impl std::fmt::Debug) -> Self {
        Self::FinalizationActorTaskFailed {
            reason: format!("{error:?}"),
        }
    }

    /// Creates an error when the peer manager actor exits.
    pub fn peer_manager_actor_exited(error: impl std::fmt::Debug) -> Self {
        Self::PeerManagerActorExited {
            reason: format!("{error:?}"),
        }
    }

    /// Creates an error when the marshal actor exits.
    pub fn marshal_actor_exited(error: impl std::fmt::Debug) -> Self {
        Self::MarshalActorExited {
            reason: format!("{error:?}"),
        }
    }

    /// Creates an error when the mandatory teeAttestationV1 ChainSpec is invalid.
    pub fn invalid_tee_attestation_chain_spec(error: impl std::fmt::Display) -> Self {
        Self::InvalidTeeAttestationChainSpec {
            reason: error.to_string(),
        }
    }

    /// Creates an error when the Reth P2P signing key is invalid.
    pub fn invalid_reth_p2p_signing_key(error: impl std::fmt::Display) -> Self {
        Self::InvalidRethP2pSigningKey {
            reason: error.to_string(),
        }
    }

    /// Creates an error when reconnecting to the production NodeHost enclave fails.
    pub fn production_node_host_reconnect_failed(error: impl std::fmt::Display) -> Self {
        Self::ProductionNodeHostReconnectFailed {
            reason: error.to_string(),
        }
    }

    /// Creates an error when loading the committed NodeHost manifest fails.
    pub fn committed_node_host_manifest_load_failed(error: impl std::fmt::Display) -> Self {
        Self::CommittedNodeHostManifestLoadFailed {
            reason: error.to_string(),
        }
    }

    /// Creates an error when reconnecting to the GramineDirectDev enclave fails.
    pub fn gramine_direct_dev_reconnect_failed(error: impl std::fmt::Display) -> Self {
        Self::GramineDirectDevReconnectFailed {
            reason: error.to_string(),
        }
    }

    /// Creates an error when the startup TEE DKG ceremony fails.
    pub fn tee_dkg_ceremony_failed(error: impl std::fmt::Display) -> Self {
        Self::TeeDkgCeremonyFailed {
            reason: error.to_string(),
        }
    }

    /// Creates an error when the startup TEE bootstrap coordination fails.
    pub fn tee_bootstrap_coordination_failed(error: impl std::fmt::Display) -> Self {
        Self::TeeBootstrapCoordinationFailed {
            reason: error.to_string(),
        }
    }

    /// Creates an error when registering the initial DKG subchannel fails.
    pub fn register_initial_dkg_subchannel(error: impl std::fmt::Display) -> Self {
        Self::RegisterInitialDkgSubchannel {
            reason: error.to_string(),
        }
    }

    /// Creates an error when the participant set is invalid.
    pub fn invalid_participant_set(error: impl std::fmt::Display) -> Self {
        Self::InvalidParticipantSet {
            reason: error.to_string(),
        }
    }

    /// Creates an error when pre-registering TEE DKG round 0 fails.
    pub fn pre_register_tee_dkg_round0(error: impl std::fmt::Display) -> Self {
        Self::PreRegisterTeeDkgRound0 {
            reason: error.to_string(),
        }
    }

    /// Creates an error when pre-registering TEE bootstrap round 0 fails.
    pub fn pre_register_tee_bootstrap_round0(error: impl std::fmt::Display) -> Self {
        Self::PreRegisterTeeBootstrapRound0 {
            reason: error.to_string(),
        }
    }

    /// Creates an error when reading the provider block hash at the consensus tip fails.
    pub fn read_provider_block_hash_at_consensus_tip(
        height: u64,
        error: impl std::fmt::Display,
    ) -> Self {
        Self::ReadProviderBlockHashAtConsensusTip {
            height,
            reason: error.to_string(),
        }
    }

    /// Creates an error when refreshing the frozen validator set fails.
    pub fn refresh_frozen_validator_set_failed(
        freeze_height: u64,
        error: impl std::fmt::Display,
    ) -> Self {
        Self::RefreshFrozenValidatorSetFailed {
            freeze_height,
            reason: error.to_string(),
        }
    }
}
