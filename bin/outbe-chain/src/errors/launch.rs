//! Typed error definitions for node launch, admission, and shutdown.

use std::sync::Arc;

use alloy_primitives::B256;
use outbe_primitives::projection::ProjectionFailureClass;
use outbe_primitives::tee_attestation_v1::AttestationMode;

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
    /// Indicates that validator recovery requires certified follower catch-up first.
    #[error(
        "validator recovery requires certified follower catch-up before authority startup: local finalized state has not reached the durable TEE join anchor at height {height} ({hash}) in {datadir}. Stop this process; start the same outbe-chain binary with this same datadir and the same network/TEE options, omit --validator and every validator signing/Radicle authority flag, and add --upstream <healthy-certified-rpc> (do not use --upstream.nocertify). Wait for `local TEE lease guard armed at authenticated catch-up anchor`, stop the follower, then restart the original validator command. Submit readiness only after the validator is caught up; signing authority returns only after a fresh DKG installs current private material"
    )]
    ValidatorRecoveryCatchUpRequired {
        /// Durable join anchor height.
        height: u64,
        /// Durable join anchor block hash.
        hash: B256,
        /// Node data directory display string.
        datadir: String,
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
    /// Indicates that the OCOMP bundle catalog is not a real directory.
    #[error("OCOMP bundle catalog must be a real directory")]
    BundleCatalogNotRealDirectory,
    /// Indicates that an OCOMP bundle catalog entry is not a regular file.
    #[error("OCOMP bundle catalog entries must be regular files")]
    BundleCatalogEntryNotRegularFile,
    /// Indicates that an OCOMP bundle filename is not valid UTF-8.
    #[error("OCOMP bundle filename is not UTF-8")]
    BundleFilenameNotUtf8,
    /// Indicates that an OCOMP bundle filename lacks the `.ocb1` extension.
    #[error("OCOMP bundle filename must end in .ocb1")]
    BundleFilenameMissingExtension,
    /// Indicates that an OCOMP bundle filename is not 64 lowercase hex characters.
    #[error("OCOMP bundle filename must be 64 lowercase hex characters plus .ocb1")]
    BundleFilenameInvalidHex,
    /// Indicates that an installed OCOMP bundle filename does not match its canonical hash.
    #[error("installed OCOMP bundle filename does not match its canonical hash")]
    BundleFilenameHashMismatch,
    /// Indicates conflicting installed OCOMP bundle bytes.
    #[error("conflicting installed OCOMP bundle bytes")]
    ConflictingBundleBytes,
    /// Indicates that configured OCOMP bundle hashes require a populated catalog.
    #[error("configured OCOMP bundle hashes require a populated hash-addressed catalog")]
    ConfiguredBundlesRequirePopulatedCatalog,
    /// Indicates that a configured OCOMP bundle hash is not installed in the catalog.
    #[error("configured OCOMP bundle {hash} is not installed in the catalog")]
    ConfiguredBundleNotInstalled {
        /// Missing bundle hash.
        hash: B256,
    },
    /// Indicates an unsupported number of OCOMP runtime bundles.
    #[error("OCOMP runtime supports exactly active plus one staged/retiring bundle")]
    InvalidRuntimeBundleCount,
    /// Indicates that the OCOMP bundle catalog does not match `OCOMP_PROTOCOL_BUNDLE_HASHES`.
    #[error("OCOMP bundle catalog must exactly match OCOMP_PROTOCOL_BUNDLE_HASHES")]
    BundleCatalogMismatchEnv,
    /// Indicates that a two-bundle catalog requires `OCOMP_PROTOCOL_BUNDLE_HASHES` for ordering.
    #[error("OCOMP_PROTOCOL_BUNDLE_HASHES is required to order a post-genesis two-bundle catalog")]
    TwoBundleCatalogRequiresEnvOrder,
    /// Indicates that an OCOMP bundle hash lacks a `0x` prefix.
    #[error("OCOMP bundle hash must have a 0x prefix")]
    BundleHashMissingPrefix,
    /// Indicates that an OCOMP bundle hash is not 64 lowercase hex characters after `0x`.
    #[error("OCOMP bundle hash must be 64 lowercase hex characters after 0x")]
    BundleHashInvalidHex,
    /// Indicates a duplicate hash in the OCOMP bundle hash list.
    #[error("OCOMP bundle hash list contains a duplicate")]
    DuplicateBundleHash,
    /// Indicates an invalid length for the OCOMP bundle hash list.
    #[error("OCOMP bundle hash list must contain one or two adjacent authorities")]
    InvalidBundleHashListLength,
    /// Indicates that plain EL full-node mode without `--upstream` is disabled.
    #[error(
        "ADR-005 plain EL full-node mode is disabled: use --upstream so historical execution is gated by exact finalized-parent projection readiness"
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
    /// Indicates an embedded OCOMP runtime failure.
    #[error("embedded OCOMP failure ({class:?}): {message}")]
    EmbeddedOcompFailure {
        /// Projection failure class.
        class: ProjectionFailureClass,
        /// Failure message.
        message: Arc<str>,
    },
    /// Indicates that the embedded OCOMP exit channel closed without a verdict.
    #[error("embedded OCOMP exit channel closed without a verdict")]
    EmbeddedOcompExitChannelClosed,
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
    /// Indicates that the node data directory has no OCOMP domain parent.
    #[error("node data directory has no OCOMP domain parent")]
    MissingOcompDomainParent,
    /// Indicates that the consensus port leaves no OCOMP Worker endpoint port.
    #[error("consensus port leaves no OCOMP Worker endpoint port")]
    MissingOcompWorkerPort,
    /// Indicates that the OCOMP worker limit exceeds `u16`.
    #[error("OCOMP worker limit exceeds u16")]
    OcompWorkerLimitExceedsU16,
    /// Indicates an overflow in the OCOMP bundle lane port stride.
    #[error("OCOMP bundle lane port stride overflow")]
    OcompLanePortStrideOverflow,
    /// Indicates that the OCOMP bundle lane count exceeds `u16`.
    #[error("OCOMP bundle lane count exceeds u16")]
    OcompLaneCountExceedsU16,
    /// Indicates an overflow in the OCOMP bundle lane port offset.
    #[error("OCOMP bundle lane port offset overflow")]
    OcompLanePortOffsetOverflow,
    /// Indicates that an OCOMP bundle lane leaves no Worker endpoint port.
    #[error("OCOMP bundle lane leaves no Worker endpoint port")]
    OcompLaneMissingWorkerPort,
    /// Indicates that validator OCOMP requires the local HTTP RPC server.
    #[error("validator OCOMP requires the local HTTP RPC server")]
    ValidatorOcompRequiresHttpRpc,
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
}

/// Represents errors when building OCOMP Lysis openings over RPC.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LysisOpeningsError {
    /// Indicates that decoding the OCOMP openings request failed.
    #[error("decode OCOMP openings request: {details}")]
    DecodeRequest {
        /// Decode failure details.
        details: String,
    },
    /// Indicates that reading the finalized head failed.
    #[error("read finalized head: {details}")]
    ReadFinalizedHead {
        /// Provider failure details.
        details: String,
    },
    /// Indicates that the finalized head is unavailable.
    #[error("finalized head is unavailable")]
    FinalizedHeadUnavailable,
    /// Indicates that reading the finalized OCOMP job failed.
    #[error("read finalized OCOMP job: {details}")]
    ReadFinalizedJob {
        /// Storage read failure details.
        details: String,
    },
    /// Indicates that the OCOMP job is not finalized.
    #[error("OCOMP job is not finalized")]
    JobNotFinalized,
    /// Indicates that the OCOMP job is not available for calculation or replay.
    #[error("OCOMP job is not available for calculation or replay")]
    JobNotAvailable,
    /// Indicates a `JobId` mismatch in the OCOMP openings request.
    #[error("OCOMP openings request JobId mismatch")]
    RequestJobIdMismatch,
    /// Indicates that deriving the input lease failed.
    #[error("derive input lease: {details}")]
    DeriveInputLease {
        /// Derivation failure details.
        details: String,
    },
    /// Indicates that building exact OCOMP openings failed.
    #[error("build exact OCOMP openings: {details}")]
    BuildOpenings {
        /// Openings build failure details.
        details: String,
    },
    /// Indicates a `JobId` mismatch in the built OCOMP openings.
    #[error("OCOMP openings JobId mismatch")]
    OpeningsJobIdMismatch,
    /// Indicates that encoding the OCOMP openings failed.
    #[error("encode OCOMP openings: {details}")]
    EncodeOpenings {
        /// Encoding failure details.
        details: String,
    },
}

impl LysisOpeningsError {
    /// Converts this openings error into an RPC error string.
    pub(crate) fn into_rpc_error(self) -> std::string::String {
        self.to_string()
    }
}
