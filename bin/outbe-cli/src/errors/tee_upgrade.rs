//! Typed errors for TEE same-platform and network upgrade workflows.

use super::TeeIdentityError;

/// Errors for TEE upgrade commands on the same platform.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeUpgradeError {
    /// Loading the committed NodeHost manifest failed.
    #[error("load committed NodeHost manifest: {0}")]
    LoadCommittedManifest(outbe_tee::TransportError),
    /// Finalized chain state has no staged successor TEE policy.
    #[error("no successor TEE policy is staged at finalized state")]
    NoStagedSuccessorPolicy,
    /// Hashing the staged successor policy from finalized state failed.
    #[error("hash finalized staged successor policy: {0}")]
    HashSuccessorPolicy(outbe_primitives::tee_attestation_v1::CodecError),
    /// Hashing the active NodeHost manifest failed.
    #[error("hash active NodeHost manifest: {0}")]
    HashActiveManifest(outbe_primitives::tee_attestation_v1::CodecError),
    /// Hashing the candidate NodeHost manifest failed.
    #[error("hash candidate NodeHost manifest: {0}")]
    HashCandidateManifest(outbe_primitives::tee_attestation_v1::CodecError),
    /// No upgrade journal entry exists in the node data directory.
    #[error("no upgrade is prepared")]
    NoUpgradePrepared,
    /// Deriving the authorization hash from the committed manifest failed.
    #[error("invalid manifest: {0}")]
    InvalidManifest(outbe_primitives::tee_attestation_v1::CodecError),
    /// The committed candidate manifest has no finalized upgrade checkpoint.
    #[error("committed candidate has no finalized upgrade checkpoint")]
    MissingFinalizedUpgradeCheckpoint,
    /// Finalizing an upgrade requires a submitted transition state.
    #[error("upgrade-finalize requires a submitted transition")]
    UpgradeNotSubmitted,
    /// The durable submission file for the replacement candidate is missing.
    #[error("durable transition evidence is missing")]
    MissingDurableTransitionEvidence,
    /// Decoding the durable evidence for the replacement candidate failed.
    #[error("invalid durable transition evidence: {0}")]
    InvalidDurableTransitionEvidence(outbe_primitives::tee_attestation_v1::CodecError),
    /// The transition binding did not finalize before the timeout expired.
    #[error("transition is not finalized; checkpoint retained for retry")]
    TransitionNotFinalized,
    /// `tee upgrade-submit` requires the global `--private-key` flag.
    #[error("tee upgrade-submit requires the global --private-key EVM signer")]
    SubmitPrivateKeyRequired,
    /// Signing the upgrade submission with the node authority key failed.
    #[error("node authority signing failed: {0}")]
    NodeAuthoritySigningFailed(TeeIdentityError),
    /// The chain identifier in the committed manifest differs from `eth_chainId`.
    #[error("committed NodeHost manifest chain id does not match eth_chainId")]
    ManifestChainIdMismatch,
    /// NodeHost upgrade commands require `--reth-p2p-secret-key`.
    #[error("NodeHost upgrade requires --reth-p2p-secret-key")]
    MissingRethP2pSecretKey,
    /// Preparing the replacement candidate enclave failed.
    #[error("prepare NodeHost candidate B: {0}")]
    PrepareCandidateB(outbe_tee::TransportError),
}

/// Errors for `tee upgrade-provision` and network key provisioning.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeUpgradeNetworkError {
    /// `upgrade-provision` requires the `--private-key` flag.
    #[error("upgrade-provision requires --private-key")]
    PrivateKeyRequired,
    /// Hashing the candidate manifest authorization failed.
    #[error("manifest: {0}")]
    InvalidManifest(outbe_primitives::tee_attestation_v1::CodecError),
    /// The candidate enclave already holds a different network offer key.
    #[error("candidate already contains a different network key")]
    CandidateKeyMismatch,
    /// Decoding the exported onboarding artifact failed.
    #[error("export artifact: {0}")]
    DecodeExportArtifact(String),
    /// The source node returned an artifact for a different recipient context.
    #[error("source returned a different recipient")]
    SourceRecipientMismatch,
    /// The candidate enclave installed an unexpected network offer key.
    #[error("candidate installed an unexpected network key")]
    UnexpectedInstalledNetworkKey,
    /// The epoch value in the on-chain registry exceeds `u64::MAX`.
    #[error("registry epoch exceeds u64")]
    RegistryEpochOverflow,
    /// Decoding the consensus block artifacts failed.
    #[error("decode block {height} artifacts: {reason}")]
    DecodeBlockArtifacts { height: u64, reason: String },
    /// Encoding the canonical header for the committee transition failed.
    #[error("encode committee transition: {0}")]
    EncodeCommitteeTransition(outbe_tee::finalized_admission::FinalizedAdmissionCodecError),
    /// The proof count for the committee transition exceeds the protocol bound.
    #[error("upgrade committee proof exceeds count limit")]
    CommitteeProofCountExceeded,
    /// Incrementing the expected epoch for the committee transition overflowed.
    #[error("committee transition epoch overflow")]
    CommitteeEpochOverflow,
    /// The `eth_getProof` response has no `storageProof` array.
    #[error("eth_getProof has no storageProof array")]
    MissingStorageProofArray,
    /// The `eth_getProof` response omitted a requested `TeeRegistry` slot.
    #[error("eth_getProof omitted a required TeeRegistry slot")]
    OmittedRequiredRegistrySlot,
    /// Encoding the finalized witness for onboarding admission failed.
    #[error("encode finalized onboarding admission witness: {0}")]
    EncodeAdmissionWitness(outbe_tee::finalized_admission::FinalizedAdmissionCodecError),
    /// The prepare transaction did not finalize before the timeout expired.
    #[error("prepare not finalized; saved transaction retained, rerun upgrade-provision")]
    PrepareNotFinalized,
    /// The RPC server returned a transaction hash that differs from the saved hash.
    #[error("RPC returned a different prepare transaction hash")]
    PrepareTxHashMismatch,
    /// The prepare transaction reverted on-chain.
    #[error("prepare transaction reverted; saved evidence retained for diagnosis")]
    PrepareTxReverted,
    /// The saved prepare transaction does not match the current relay signer.
    #[error("saved prepare transaction does not match this relay")]
    SavedPrepareRelayMismatch,
    /// Decoding the saved onboarding context failed.
    #[error("saved context: {0}")]
    DecodeSavedContext(String),
    /// No upgrade journal entry exists yet.
    #[error("run upgrade-prepare first")]
    UpgradeNotPrepared,
    /// The connected candidate manifest differs from the prepared upgrade journal.
    #[error("connected candidate differs from prepared upgrade")]
    CandidateManifestMismatch,
    /// The upgrade journal already advanced past network key provisioning.
    #[error("candidate has already advanced to {0}; use upgrade-submit or upgrade-finalize")]
    CandidateAlreadyAdvanced(String),
    /// Decoding the saved attestation evidence for preparation failed.
    #[error("saved prepare evidence: {0}")]
    DecodeSavedPrepareEvidence(outbe_primitives::tee_attestation_v1::CodecError),
    /// The saved candidate submission uses a different `--binding-id`.
    #[error("saved candidate uses another binding ID; retain the original --binding-id")]
    SavedBindingIdMismatch,
    /// Hashing the node identifier failed.
    #[error("node: {0}")]
    NodeIdHash(outbe_primitives::tee_attestation_v1::CodecError),
    /// A live pending candidate prevents starting a `--new-attempt`.
    #[error("cancel the live candidate before --new-attempt")]
    LiveCandidateExistsForNewAttempt,
    /// The previous prepare submission is still pending and unexpired.
    #[error("previous prepare is not finalized or expired; replay it before starting another")]
    PreviousPreparePending,
    /// No approved successor policy exists in finalized state.
    #[error("no approved successor")]
    NoApprovedSuccessor,
    /// The finalized head advanced while the CLI read preparation state.
    #[error("finalized head advanced during preparation; retry")]
    FinalizedHeadAdvanced,
    /// Hashing the successor policy failed.
    #[error("policy: {0}")]
    PolicyHash(outbe_primitives::tee_attestation_v1::CodecError),
    /// The approved successor policy on-chain changed after `upgrade-prepare`.
    #[error("approved successor changed")]
    ApprovedSuccessorChanged,
    /// A live pending candidate already exists on-chain.
    #[error(
        "a live candidate already exists; recover its saved submission or cancel it explicitly"
    )]
    LiveCandidateAlreadyExists,
    /// Incrementing the transition prepare nonce overflowed.
    #[error("prepare nonce exhausted")]
    PrepareNonceExhausted,
    /// Encoding the canonical transition evidence failed.
    #[error("evidence: {0}")]
    EncodeEvidence(outbe_primitives::tee_attestation_v1::CodecError),
    /// Hashing the transition intent failed.
    #[error("intent: {0}")]
    IntentHash(outbe_primitives::tee_attestation_v1::CodecError),
    /// Computing the maximum gas for `prepareEnclaveUpgrade` failed.
    #[error("prepare gas: {0}")]
    PrepareGas(outbe_primitives::tee_attestation_v1::CodecError),
    /// The relay account balance cannot cover the prepare transaction gas.
    #[error("insufficient balance for prepare transaction")]
    InsufficientRelayBalance,
    /// Subtracting the collateral safety margin underflowed.
    #[error("collateral safety margin")]
    CollateralSafetyMargin,
    /// The requested validity timestamp for the candidate is not after finalized time.
    #[error("candidate already expired")]
    CandidateAlreadyExpired,
    /// The candidate lease falls outside policy or collateral validity bounds.
    #[error("candidate lease is outside policy/collateral limits; choose a valid --valid-until")]
    CandidateLeaseOutOfBounds,
}
