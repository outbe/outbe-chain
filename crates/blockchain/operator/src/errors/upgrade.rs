//! Typed errors for TEE upgrade checkpoints and transition calls.

use std::path::PathBuf;

use alloy_primitives::{Address, U256};
use outbe_primitives::tee_attestation_v1::CodecError;
use outbe_tee::{dcap_protocol::DcapRejectCodeV1, TransportError};
use thiserror::Error;

/// Errors from enclave upgrade checkpoints and relay calls.
#[derive(Debug, Error)]
pub enum UpgradeError {
    /// The snapshot has an unsupported version or zero generation.
    #[error("unsupported upgrade journal version or generation")]
    UnsupportedVersionOrGeneration,

    /// Another enclave upgrade is already active in the journal.
    #[error("another enclave upgrade is already journaled")]
    UpgradeAlreadyJournaled,

    /// A pending renewal blocks a new enclave upgrade.
    #[error("cannot prepare enclave upgrade while an exact renewal submission is pending")]
    PendingRenewalBlocksPrepare,

    /// The reader did not find a committed checkpoint.
    #[error("{0} disappeared")]
    CheckpointDisappeared(&'static str),

    /// The journal has no prepared upgrade candidate.
    #[error("upgrade candidate is not prepared")]
    CandidateNotPrepared,

    /// The candidate sealed root changed after the checkpoint.
    #[error("candidate sealed root changed after its durable checkpoint")]
    CandidateSealedRootChanged,

    /// The sealed root copy requires the `candidatePrepared` state.
    #[error("sealed root can be copied only at candidate-prepared checkpoint")]
    CopyRequiresCandidatePrepared,

    /// The journal is not ready to provision a network key.
    #[error("upgrade is not prepared")]
    UpgradeNotPrepared,

    /// A pending renewal blocks the key readiness checkpoint.
    #[error(
        "key is sealed; finish pending renewal before checkpointing transition readiness, then rerun upgrade-provision"
    )]
    PendingRenewalBlocksProvision,

    /// The candidate seal lacks the combined SGX header.
    #[error("network-provisioned candidate must use combined SGX sealing")]
    InvalidCombinedSgxSeal,

    /// The provisioned seal bytes changed after the checkpoint.
    #[error("provisioned seal changed after checkpoint")]
    ProvisionedSealChanged,

    /// The encoded network submission exceeds the size limit.
    #[error("network submission exceeds cap")]
    NetworkSubmissionExceedsCap,

    /// The verifier rejected the candidate key-ready proof.
    #[error("candidate key-ready proof is invalid: {0}")]
    InvalidKeyReadyProof(CodecError),

    /// The encoder rejected the candidate key-ready proof.
    #[error("encode candidate key-ready proof: {0}")]
    EncodeKeyReadyProof(CodecError),

    /// The key-ready step requires the `keyProvisioned` checkpoint.
    #[error("candidate key readiness requires the key-provisioned checkpoint")]
    KeyReadyRequiresKeyProvisioned,

    /// The proof manifest hash disagrees with the journaled context.
    #[error("candidate key-ready proof does not match the journaled manifest")]
    KeyReadyProofManifestMismatch,

    /// The submission step requires the `candidateKeyReady` checkpoint.
    #[error("upgrade submission requires candidate-key-ready checkpoint")]
    SubmissionRequiresKeyReady,

    /// The relay step requires the `submissionPrepared` checkpoint.
    #[error("upgrade relay requires submission-prepared checkpoint")]
    RelayRequiresSubmissionPrepared,

    /// The finalized authority has a zero height or hash.
    #[error("finalized upgrade authority is incomplete")]
    IncompleteFinalizedAuthority,

    /// The finalization step requires the `submitted` checkpoint.
    #[error("upgrade finalization requires a submitted checkpoint")]
    FinalizationRequiresSubmitted,

    /// The promotion step requires the `finalized` checkpoint.
    #[error("upgrade promotion requires a finalized checkpoint")]
    PromotionRequiresFinalized,

    /// The finalized height is below the activation cutoff.
    #[error("successor activation cutoff has not been reached")]
    CutoffNotReached,

    /// A finalized upgrade cannot enter the missed-cutoff state.
    #[error("a finalized upgrade cannot become a missed-cutoff terminal")]
    FinalizedCannotMissCutoff,

    /// The submission checkpoint has no durable evidence file.
    #[error("upgrade submission checkpoint has no durable evidence")]
    MissingDurableSubmissionEvidence,

    /// The decoder rejected the durable transition evidence.
    #[error("invalid transition evidence: {0}")]
    InvalidTransitionEvidence(CodecError),

    /// The upgrade state has no sealed-root hash.
    #[error("upgrade has no sealed-root checkpoint")]
    MissingSealedRootCheckpoint,

    /// Another writer modified the upgrade journal during recovery.
    #[error("upgrade changed during expiration recovery; retry")]
    ConcurrentChangeDuringRecovery,

    /// The prepared submission contains no relay variants.
    #[error("upgrade submission has no relay transaction")]
    MissingRelayTransaction,

    /// The candidate failed to generate `GramineDirectDev` evidence.
    #[error("generate DirectDev transition evidence: {0}")]
    GenerateDirectDevEvidence(TransportError),

    /// The candidate failed to generate the DCAP quote.
    #[error("generate candidate transition quote: {0}")]
    GenerateTransitionQuote(TransportError),

    /// The collateral provider failed to fetch transition collateral.
    #[error("acquire candidate transition collateral: {0}")]
    AcquireTransitionCollateral(TransportError),

    /// The verifier rejected the transition collateral window.
    #[error("validate transition collateral: {0:?}")]
    ValidateTransitionCollateral(DcapRejectCodeV1),

    /// The transition binding id is zero or unchanged.
    #[error("measurement transition requires a fresh nonzero binding id")]
    InvalidTransitionBindingId,

    /// The hasher rejected the staged successor policy.
    #[error("hash staged successor policy: {0}")]
    HashStagedSuccessorPolicy(CodecError),

    /// The hasher failed to derive the candidate enclave id.
    #[error("derive candidate enclave id: {0}")]
    DeriveCandidateEnclaveId(CodecError),

    /// The binding version counter overflowed `u64`.
    #[error("binding version exhausted")]
    BindingVersionExhausted,

    /// The registration version counter overflowed `u64`.
    #[error("registration version exhausted")]
    RegistrationVersionExhausted,

    /// The transition nonce counter overflowed `u64`.
    #[error("transition nonce exhausted")]
    TransitionNonceExhausted,

    /// The candidate manifest disagrees with the transition intent.
    #[error("candidate manifest does not bind transition intent: {0}")]
    CandidateManifestIntentMismatch(CodecError),

    /// The active sealed root file is empty.
    #[error("active sealed root is empty")]
    EmptyActiveSealedRoot,

    /// The destination sealed root file has different bytes.
    #[error("candidate sealed root already exists with different bytes")]
    ConflictingCandidateSealedRoot,

    /// The upgrade context has missing hashes or matching paths.
    #[error("upgrade context is incomplete or self-referential")]
    InvalidUpgradeContext,

    /// The decoder rejected the saved prepare evidence.
    #[error("saved prepare evidence: {0}")]
    SavedPrepareEvidence(CodecError),

    /// The decoder rejected the saved prepare context.
    #[error("saved prepare context: {0:?}")]
    SavedPrepareContext(DcapRejectCodeV1),

    /// The saved node signature does not have 65 bytes.
    #[error("saved node signature length")]
    SavedNodeSignatureLength,

    /// The saved enclave signature does not have 64 bytes.
    #[error("saved enclave signature length")]
    SavedEnclaveSignatureLength,

    /// The hasher rejected the saved prepare intent.
    #[error("saved intent: {0}")]
    SavedIntentHash(CodecError),

    /// The hasher rejected the saved prepare node id.
    #[error("saved node: {0}")]
    SavedNodeHash(CodecError),

    /// The saved network submission has inconsistent commitments or signatures.
    #[error("saved network upgrade commitments or signatures are inconsistent")]
    SavedNetworkUpgradeMismatch,

    /// The hasher rejected the candidate node identity.
    #[error("hash candidate node identity: {0}")]
    HashCandidateNodeIdentity(CodecError),

    /// The hasher failed to derive the candidate enclave identity.
    #[error("derive candidate enclave identity: {0}")]
    DeriveCandidateEnclaveIdentity(CodecError),

    /// The hasher failed to derive the candidate authorization hash.
    #[error("derive candidate NodeHost authorization: {0}")]
    DeriveCandidateAuthorization(CodecError),

    /// The candidate manifest is not a valid successor for A.
    #[error("candidate manifest is not a same-NodeHost successor of finalized A")]
    NotSameNodeHostSuccessor,

    /// The upgrade selector disagrees with the candidate node identity.
    #[error("upgrade selector does not match the candidate node identity")]
    SelectorMismatch,

    /// The hasher rejected the transition node identity.
    #[error("hash transition node identity: {0}")]
    HashTransitionNodeIdentity(CodecError),

    /// The finalized binding matches neither the source nor the target.
    #[error("finalized Registry binding matches neither transition source nor target")]
    BindingNeitherSourceNorTarget,

    /// The submission has missing hashes or invalid variant counts.
    #[error("upgrade submission is incomplete or exceeds its variant cap")]
    InvalidUpgradeSubmissionBounds,

    /// The first relay variant disagrees with the submission relay.
    #[error("upgrade submission relay binding mismatch")]
    SubmissionRelayBindingMismatch,

    /// A relay variant has inconsistent fields or hashes.
    #[error("upgrade submission contains a competing relay variant")]
    CompetingSubmissionRelayVariant,

    /// The missed-cutoff height precedes the activation height.
    #[error("terminal cutoff checkpoint precedes policy activation")]
    CutoffPrecedesActivation,

    /// The submission checkpoint omits its security material.
    #[error("submission checkpoint has no security material")]
    MissingCheckpointSecurityMaterial,

    /// The submission checkpoint omits its submission record.
    #[error("submission checkpoint has no submission")]
    MissingCheckpointSubmission,

    /// The submitted hash list disagrees with the relay variants.
    #[error("submitted upgrade transaction list is non-canonical")]
    NonCanonicalSubmittedTransactionList,

    /// The finalized checkpoint has a zero block hash.
    #[error("finalized upgrade checkpoint has a zero block hash")]
    ZeroFinalizedCheckpointHash,

    /// The journal state machine rejected the checkpoint transition.
    #[error("invalid upgrade checkpoint transition {from} -> {to}")]
    InvalidCheckpointTransition {
        from: &'static str,
        to: &'static str,
    },

    /// Security material hashes changed between checkpoints.
    #[error("upgrade security material changed across checkpoints")]
    SecurityMaterialChanged,

    /// The expired submission reset changed the sealed-root hash.
    #[error("expired submission recovery changed the sealed root")]
    ExpiredRecoveryChangedSealedRoot,

    /// The provisioned checkpoint has a zero sealed-root hash.
    #[error("upgrade checkpoint has a zero sealed-root hash")]
    ZeroSealedRootHash,

    /// The key-ready checkpoint has a zero offer key or proof.
    #[error("key-ready checkpoint has a zero offer key or proof hash")]
    ZeroKeyReadyMaterial,

    /// The hasher rejected the candidate manifest.
    #[error("hash candidate manifest: {0}")]
    HashCandidateManifest(CodecError),

    /// The connected candidate manifest disagrees with the journaled hash.
    #[error("connected candidate differs from the journaled candidate manifest")]
    ConnectedCandidateManifestMismatch,

    /// Finalized state has no staged or active successor policy.
    #[error("no successor TEE policy is staged at finalized state")]
    NoStagedSuccessorPolicy,

    /// The active binding and staged policy use different finalized heads.
    #[error("finalized active binding and staged policy were read at different heads")]
    FinalizedHeadMismatch,

    /// The hasher rejected the active finalized policy.
    #[error("hash active finalized policy: {0}")]
    HashActiveFinalizedPolicy(CodecError),

    /// The staged policy does not directly succeed the active policy.
    #[error("staged successor is not the direct successor of the finalized active policy")]
    InvalidDirectSuccessorPolicy,

    /// The hasher rejected the successor policy.
    #[error("invalid successor: {0}")]
    InvalidSuccessorPolicy(CodecError),

    /// The reader failed to load the durable candidate submission.
    #[error("reload durable candidate submission: {0}")]
    ReloadDurableCandidateSubmission(TransportError),

    /// The decoder rejected the durable transition evidence.
    #[error("invalid durable transition: {0}")]
    InvalidDurableTransition(CodecError),

    /// The expired transition already executed on-chain.
    #[error("expired transition already executed; finalize it before renewing")]
    ExpiredTransitionAlreadyExecuted,

    /// The hasher rejected the durable transition intent.
    #[error("invalid intent: {0}")]
    InvalidTransitionIntent(CodecError),

    /// The collateral margin exceeds the collateral expiration.
    #[error("transition collateral margin underflows")]
    TransitionCollateralMarginUnderflow,

    /// The minimum transition lease overflowed `u64`.
    #[error("minimum transition lease overflows")]
    MinimumTransitionLeaseOverflow,

    /// The fresh Intel collateral cannot cover the minimum lease.
    #[error("fresh Intel collateral cannot satisfy the successor minimum lease")]
    CollateralBelowMinimumLease,

    /// The hasher rejected the transition intent.
    #[error("hash transition intent: {0}")]
    HashTransitionIntent(CodecError),

    /// The writer failed to save the candidate submission.
    #[error("persist exact candidate submission: {0}")]
    PersistCandidateSubmission(TransportError),

    /// The candidate transition evidence omits the key-ready proof.
    #[error("candidate transition evidence has no key-ready proof")]
    MissingCandidateKeyReadyProof,

    /// The requested transition lease already expired at finalized time.
    #[error("transition lease is already expired at finalized time")]
    TransitionLeaseAlreadyExpired,

    /// The candidate evidence violates the successor lease window.
    #[error("candidate evidence cannot satisfy the staged successor lease window")]
    EvidenceViolatesLeaseWindow,

    /// The decoder rejected the durable transition evidence.
    #[error("decode durable transition evidence: {0}")]
    DecodeDurableTransitionEvidence(CodecError),

    /// The durable submission targets another policy or operation.
    #[error("durable candidate submission targets another transition policy")]
    DurableSubmissionPolicyMismatch,

    /// The durable transition evidence omits the key-ready proof.
    #[error("durable transition evidence has no key-ready proof")]
    MissingDurableKeyReadyProof,

    /// The verifier rejected the durable key-ready proof.
    #[error("durable key-ready proof is invalid: {0}")]
    InvalidDurableKeyReadyProof(CodecError),

    /// The gas schedule rejected the transition call parameters.
    #[error("calculate normative transition gas: {0}")]
    CalculateNormativeTransitionGas(CodecError),

    /// The relay balance cannot cover the transition gas cost.
    #[error("upgrade relay {address} has {balance} but needs at least {required_balance}")]
    InsufficientRelayBalance {
        address: Address,
        balance: U256,
        required_balance: U256,
    },

    /// The relay step requires the `candidateKeyReady` checkpoint.
    #[error("upgrade relay requires candidate-key-ready checkpoint")]
    RelayRequiresCandidateKeyReady,

    /// The reader failed to load the exact candidate submission.
    #[error("reload exact candidate submission: {0}")]
    ReloadExactCandidateSubmission(TransportError),

    /// The `candidateKeyReady` checkpoint has no durable submission file.
    #[error("candidate-key-ready checkpoint has no durable submission")]
    KeyReadyMissingDurableSubmission,

    /// The encoder rejected the durable key-ready proof.
    #[error("encode durable key-ready proof: {0}")]
    EncodeDurableKeyReadyProof(CodecError),

    /// The durable proof disagrees with the journaled security material.
    #[error("durable transition proof differs from the journaled key-ready checkpoint")]
    DurableProofSecurityMismatch,

    /// The submission checkpoint has no durable NodeHost file.
    #[error("submission checkpoint has no durable NodeHost material")]
    SubmissionMissingDurableMaterial,

    /// The durable NodeHost material disagrees with the relay checkpoint.
    #[error("NodeHost transition material differs from the relay checkpoint")]
    NodeHostRelayMaterialMismatch,

    /// The hasher rejected the durable transition intent.
    #[error("hash durable transition intent: {0}")]
    HashDurableTransitionIntent(CodecError),

    /// The hasher rejected the durable transition evidence.
    #[error("hash durable transition evidence: {0:?}")]
    HashDurableTransitionEvidence(CodecError),

    /// Another process holds the upgrade journal lock.
    #[error("another upgrade operator owns the journal lock")]
    JournalLocked,

    /// The upgrade context changed after preparation.
    #[error("upgrade journal context cannot change after preparation")]
    ContextChangedAfterPreparation,

    /// The upgrade journal generation counter overflowed `u64`.
    #[error("upgrade journal generation exhausted")]
    GenerationExhausted,

    /// The upgrade directory path has no parent directory.
    #[error("upgrade directory has no parent")]
    DirectoryHasNoParent,

    /// The private directory violates owner-only `0700` permissions.
    #[error("private directory {0} is not owner-only 0700")]
    InvalidPrivateDirectoryPermissions(PathBuf),

    /// The private file violates owner or size bounds.
    #[error("private file {0} violates owner or size bounds")]
    InvalidPrivateFileBounds(PathBuf),

    /// The private file exceeds its size cap.
    #[error("private file {0} exceeds its size cap")]
    PrivateFileSizeCapExceeded(PathBuf),

    /// The candidate key is not provisioned yet.
    #[error("candidate key is not provisioned; run upgrade-provision")]
    CandidateKeyNotProvisioned,

    /// The upgrade missed the successor policy activation cutoff.
    #[error(
        "upgrade missed successor activation cutoff {activation_height} at finalized height {finalized_height}"
    )]
    MissedActivationCutoff {
        activation_height: u64,
        finalized_height: u64,
    },

    /// The current checkpoint is not a terminal submission state.
    #[error("upgrade checkpoint is not a terminal submission state")]
    NonTerminalSubmissionState,

    /// The prepared submission contains no signed relay bytes.
    #[error("upgrade submission has no relay bytes")]
    MissingRelayBytes,

    /// The RPC server returned a different transition hash.
    #[error("RPC returned a transaction hash different from the signed transition bytes")]
    SubmittedTransitionHashMismatch,

    /// The RPC server returned a different hash during replay.
    #[error("RPC returned a different replay transaction hash")]
    ReplayTransactionHashMismatch,
}
