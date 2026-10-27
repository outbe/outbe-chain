//! Typed error definitions for node launch, admission, and shutdown.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationMode;
use outbe_slashindicator::errors::PercentAdmissionError;

/// Represents errors during node launch, TEE admission, and shutdown.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LaunchError {
    /// Indicates that the TEE admission anchor hash does not match the local finalized block.
    #[error(
        "TEE admission anchor hash mismatch at height {height}: expected {expected}, local {local}"
    )]
    TeeAdmissionAnchorHashMismatch {
        /// Finalized block height.
        height: u64,
        /// Expected block hash.
        expected: B256,
        /// Local block hash.
        local: B256,
    },
    /// Indicates that the TEE admission anchor has bootstrap-pending state.
    #[error("TEE admission anchor unexpectedly has bootstrap-pending TEE state")]
    TeeAdmissionAnchorBootstrapPending,
    /// Indicates that the TEE admission anchor rejected the local identity.
    #[error("TEE admission anchor rejected the local identity: {reason}")]
    TeeAdmissionAnchorRejected {
        /// Rejection reason.
        reason: String,
    },
    /// Indicates that a validator without `--upstream` has an unarmed TEE lease guard.
    #[error(
        "validator recovery requires UpstreamBootstrap before authority startup: the local TEE lease guard has no validated admission anchor in the local finalized state (pending anchor height {height}, hash {hash}) in {datadir}. Restart the same validator command with this same datadir and add --upstream <healthy-certified-rpc>. Do not use --upstream.nocertify. The node then syncs through the upstream and starts the validator stack in the same process after the local TEE lease guard is armed. Submit readiness only after the validator is caught up. Signing authority returns only after a fresh DKG installs current private material"
    )]
    ValidatorRecoveryCatchUpRequired {
        /// Pending anchor height, or zero without an anchor.
        height: u64,
        /// Pending anchor block hash, or zero without an anchor.
        hash: B256,
        /// Node data directory display string.
        datadir: String,
    },
    /// Indicates that the durable TEE join anchor is above the upstream anchor.
    #[error(
        "durable TEE join anchor at height {durable} is above the upstream finalized admission anchor at height {upstream}. UpstreamBootstrap stops"
    )]
    TeeJoinAnchorAboveUpstreamAnchor {
        /// Durable join anchor height.
        durable: u64,
        /// Upstream admission anchor height.
        upstream: u64,
    },
    /// Indicates that deriving the local NodeHost identity failed.
    #[error("derive local NodeHost identity: {details}")]
    DeriveLocalNodeHostIdentity {
        /// Failure details.
        details: String,
    },
    /// Indicates a chain ID mismatch in the finalized join admission anchor.
    #[error("finalized join admission anchor chain id mismatch")]
    JoinAdmissionAnchorChainIdMismatch,
    /// Indicates a genesis hash mismatch in the finalized join admission anchor.
    #[error("finalized join admission anchor genesis mismatch")]
    JoinAdmissionAnchorGenesisMismatch,
    /// Indicates a NodeHost identity mismatch in the finalized join admission anchor.
    #[error("finalized join admission anchor NodeHost identity mismatch")]
    JoinAdmissionAnchorNodeHostMismatch,
    /// Indicates an enclave identity mismatch in the finalized join admission anchor.
    #[error("finalized join admission anchor enclave identity mismatch")]
    JoinAdmissionAnchorEnclaveMismatch,
    /// Indicates that the exact TEE lease admission header is unavailable.
    #[error("exact TEE lease admission header is unavailable")]
    TeeAdmissionHeaderUnavailable,
    /// Indicates a header hash mismatch at the finalized TEE lease marker.
    #[error("finalized TEE lease header hash mismatch: marker {marker}, header {header}")]
    FinalizedTeeLeaseHeaderHashMismatch {
        /// Marker block hash.
        marker: B256,
        /// Header block hash.
        header: B256,
    },
    /// Indicates that the finalized Registry has no binding for this FullNode.
    #[error("finalized Registry has no binding for this FullNode; run tee join first")]
    FullNodeMissingBinding,
    /// Indicates that the finalized FullNode binding does not match the local enclave.
    #[error("finalized FullNode binding does not match the committed local enclave; run tee join")]
    FullNodeBindingEnclaveMismatch,
    /// Indicates that the finalized FullNode TEE lease expired.
    #[error("finalized FullNode TEE lease expired at {valid_until}; run tee join before startup")]
    FullNodeTeeLeaseExpired {
        /// Lease expiration timestamp.
        valid_until: u64,
    },
    /// Indicates that the TEE client was unavailable during remote session retirement.
    #[error("TEE client unavailable during remote session retirement")]
    TeeClientUnavailableDuringRetirement,
    /// Indicates that the enclave did not acknowledge remote session retirement.
    #[error("enclave did not acknowledge remote session retirement")]
    EnclaveDidNotAckSessionRetirement,
    /// Indicates that the TEE lease guard stopped without returning a verdict.
    #[error("TEE lease guard stopped without a verdict")]
    TeeLeaseGuardStoppedWithoutVerdict,
    /// Indicates that plain EL full-node mode without `--upstream` is disabled.
    #[error(
        "ADR-005 plain EL full-node mode is disabled: use --upstream so historical execution uses certified finality"
    )]
    PlainElFullNodeModeDisabled,
    /// Indicates an invalid Reth P2P signing key.
    #[error("invalid Reth P2P signing key: {details}")]
    InvalidRethP2pSigningKey {
        /// Key parsing error details.
        details: String,
    },
    /// Indicates that the Reth P2P public key is not compressed SEC1-33.
    #[error("Reth P2P public key is not compressed SEC1-33")]
    RethP2pPublicKeyNotSec1,
    /// Indicates that the Radicle drain deadline expired before transport shutdown.
    #[error("Radicle drain deadline exceeded before transport shutdown")]
    RadicleDrainDeadlineExceeded,
    /// Indicates that the Radicle observer exited without completing its drain.
    #[error("Radicle observer exited without completing its drain")]
    RadicleObserverExitedWithoutDrain,
    /// Indicates that the consensus stack task failed during shutdown.
    #[error("consensus stack task failed during shutdown: {details}")]
    ConsensusStackFailedDuringShutdown {
        /// Task failure details.
        details: String,
    },
    /// Indicates that the consensus stack result deadline expired after global stop.
    #[error("consensus stack result deadline exceeded after global stop")]
    ConsensusStackResultDeadlineExceeded,
    /// Indicates that consensus graceful shutdown timed out.
    #[error("consensus graceful shutdown did not complete within 5 seconds: {details}")]
    ConsensusGracefulShutdownTimeout {
        /// Shutdown error details.
        details: String,
    },
    /// Indicates that a background task panicked during shutdown.
    #[error("{label} panicked: {details}")]
    TaskPanicked {
        /// Task label.
        label: String,
        /// Panic error details.
        details: String,
    },
    /// Indicates that the consensus stack task failed.
    #[error("consensus stack task failed: {details}")]
    ConsensusStackTaskFailed {
        /// Task failure details.
        details: String,
    },
    /// Indicates an invalid mandatory `teeAttestationV1` ChainSpec.
    #[error("invalid mandatory teeAttestationV1 ChainSpec: {details}")]
    InvalidMandatoryTeeChainSpec {
        /// Validation failure details.
        details: String,
    },
    /// Indicates an invalid `dkgPrepareWindowBlocks` value.
    #[error("invalid dkgPrepareWindowBlocks: {details}")]
    InvalidDkgPrepareWindowBlocks {
        /// Deserialization failure details.
        details: String,
    },
    /// Indicates an invalid `minBlockTimeMs` value.
    #[error("invalid minBlockTimeMs: {details}")]
    InvalidMinBlockTimeMs {
        /// Deserialization failure details.
        details: String,
    },
    /// Indicates that a validator EVM key is required for Radicle identity.
    #[error("validator EVM key is required for Radicle identity")]
    MissingValidatorEvmKeyForRadicle,
    /// Indicates that the active validator has no Radicle `NodeId` binding.
    #[error("active validator has no Radicle NodeId binding")]
    ActiveValidatorMissingRadicleNodeId,
    /// Indicates that the local Radicle `NodeId` does not match the finalized validator binding.
    #[error("local Radicle NodeId does not match finalized validator binding")]
    LocalRadicleNodeIdMismatch,
    /// Indicates that validator mode requires an EVM signer key.
    #[error("validator mode requires an EVM signer key")]
    MissingValidatorEvmSignerKey,
    /// Indicates that `--tee-enclave-socket` is required before node startup.
    #[error("mandatory {mode:?} ChainSpec requires --tee-enclave-socket before node startup")]
    MissingTeeEnclaveSocket {
        /// Active attestation mode.
        mode: AttestationMode,
    },
    /// Indicates that the TEE enclave endpoint path is not valid UTF-8.
    #[error("TEE enclave endpoint is not valid UTF-8")]
    TeeEnclaveEndpointNotUtf8,
    /// Indicates that deriving the committed enclave identity failed.
    #[error("derive committed enclave identity: {details}")]
    DeriveCommittedEnclaveIdentity {
        /// Derivation error details.
        details: String,
    },
    /// Indicates that full-node startup requires `--upstream` to authenticate the chain offer key.
    #[error("full-node startup requires --upstream to authenticate the chain offer key")]
    FullNodeRequiresUpstreamForOfferKey,
    /// Indicates that the selected upstream has no mandatory OST3 offer key.
    #[error("selected upstream has no mandatory OST3 offer key; refusing full-node startup")]
    UpstreamMissingOst3OfferKey,
    /// Indicates that the local enclave does not hold the chain's exact offer key.
    #[error(
        "local enclave does not hold the selected chain's exact offer key; refusing execution startup (no recovery or fallback)"
    )]
    LocalEnclaveOfferKeyMismatch,
    /// Indicates that the local node was rejected by finalized TEE lease state.
    #[error("local node rejected by finalized TEE lease state: {reason}")]
    LocalNodeRejectedByTeeLease {
        /// Rejection reason.
        reason: String,
    },
    /// Indicates that follower drain also failed during consensus stack shutdown.
    #[error("follower drain also failed: {details}")]
    FollowerDrainAlsoFailed {
        /// Follower drain error details.
        details: String,
    },
    /// Indicates that the consensus stack task failed while being reaped after timeout.
    #[error("consensus stack task failed while reaping: {details}")]
    ConsensusStackFailedWhileReaping {
        /// Task failure details.
        details: String,
    },
    /// Genesis slash-indicator percent is above 100 or wider than `u64`.
    #[error("genesis slash indicator percent rejected before startup: {cause}")]
    InvalidSlashIndicatorGenesisPercent {
        /// Shared admission failure. The same rule backs the runtime getters.
        cause: PercentAdmissionError,
    },
}

impl LaunchError {
    /// Wraps a follower drain failure alongside a consensus stack failure.
    pub(crate) fn follower_drain_also_failed(drain: &eyre::Report) -> Self {
        Self::FollowerDrainAlsoFailed {
            details: format!("{drain:#}"),
        }
    }

    /// Wraps a consensus stack task reaping failure after shutdown timeout.
    pub(crate) fn consensus_stack_failed_while_reaping(error: impl std::fmt::Debug) -> Self {
        Self::ConsensusStackFailedWhileReaping {
            details: format!("{error:?}"),
        }
    }
}
