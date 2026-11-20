//! Typed error definitions for node launch, admission, and shutdown.

use alloy_primitives::{Address, B256};
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
    /// Indicates that the local enclave does not hold the chain's exact offer key.
    #[error(
        "local enclave does not hold the selected chain's exact offer key; refusing execution startup (no recovery or fallback)"
    )]
    LocalEnclaveOfferKeyMismatch,
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
    /// Genesis holds storage of a store that only the chain writes when it makes a Worldwide Day.
    #[error("genesis holds storage of {address}: only the chain makes Worldwide Days")]
    GenesisHoldsDayState {
        /// Address of the store.
        address: Address,
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

/// Formats any displayable error as a string.
pub(crate) fn display_error<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

/// Admission failures retained as typed causes for the runtime authority lifecycle.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeAuthorityError {
    /// Finalized admission cannot authorize a key absent from canonical chain state.
    #[error("canonical finalized offer key is unavailable")]
    CanonicalOfferKeyUnavailable,
    /// The resident key is not the finalized chain's canonical offer key.
    #[error("resident offer key {resident} differs from finalized chain key {canonical}")]
    ResidentOfferKeyMismatch { resident: B256, canonical: B256 },
    /// Resident context inspection failed without erasing its typed cause.
    #[error("inspect resident TEE authority: {0}")]
    Resident(#[from] outbe_tee::in_process::InProcessError),
    /// Reading finalized admission facts failed without erasing its typed cause.
    #[error("read finalized TEE authority: {0}")]
    FinalizedState(#[from] outbe_primitives::error::PrecompileError),
}
