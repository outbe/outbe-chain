//! Typed errors for the TEE registry precompile and runtime.

use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Revert and fatal errors that the TEE registry produces.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TeeRegistryError {
    /// Group public key exceeds the maximum length.
    #[error("group public key too large")]
    GroupPublicKeyTooLarge,

    /// Caller attempted to bootstrap the registry a second time.
    #[error("TEE registry already bootstrapped")]
    AlreadyBootstrapped,

    /// No successor V1 policy exists in staged storage.
    #[error("no successor V1 policy is staged")]
    NoSuccessorV1PolicyStaged,

    /// Attestation evidence fails canonical decoding.
    #[error("attestation evidence is not canonical: {0}")]
    AttestationEvidenceNotCanonical(String),

    /// Attestation references a retired or unapproved policy.
    #[error("attestation targets a retired or unapproved TEE policy")]
    AttestationTargetsRetiredOrUnapprovedPolicy,

    /// Enclave upgrade requires a single-measurement predecessor policy.
    #[error("enclave upgrade requires the exact single-measurement predecessor policy")]
    UpgradeRequiresSingleMeasurementPredecessor,

    /// Enclave upgrade does not change the MRENCLAVE value.
    #[error("enclave upgrade must change MRENCLAVE")]
    UpgradeMustChangeMrenclave,

    /// Policy version increment overflows `u64`.
    #[error("TEE policy version overflow")]
    TeePolicyVersionOverflow,

    /// TEE policy fails hash calculation.
    #[error("invalid TEE policy: {0}")]
    InvalidTeePolicy(String),

    /// Verified DCAP verdict fails canonical encoding.
    #[error("verified DCAP verdict cannot be encoded: {code:#06x}")]
    VerifiedDcapVerdictEncodingFailed {
        /// Numeric error code from the canonical encoder.
        code: u16,
    },

    /// Renewal request references a different enclave identity.
    #[error("renewal targets a superseded or different enclave identity")]
    RenewalTargetsDifferentIdentity,

    /// Renewal request attempts to change the enclave measurement.
    #[error("renewal cannot replace the admitted enclave measurement")]
    RenewalCannotReplaceMeasurement,

    /// Direct-dev policy has no active measurement rule at the current height.
    #[error("GramineDirectDev policy has no active measurement projection")]
    DirectDevNoActiveMeasurement,

    /// Direct-dev policy has multiple overlapping measurement rules.
    #[error("GramineDirectDev policy has overlapping measurement projections")]
    DirectDevOverlappingMeasurements,

    /// Version or nonce counter overflows `u64`.
    #[error("{0} is exhausted")]
    CounterExhausted(&'static str),

    /// Enclave lease expired before the requested operation.
    #[error("enclave lease expired; registerEnclave rejoin is required")]
    EnclaveLeaseExpired,

    /// Active V1 policy has an invalid maximum lease duration.
    #[error("active V1 lease period is not a positive even duration")]
    InvalidActiveV1LeasePeriod,

    /// Caller submitted a renewal before the renewal window opened.
    #[error("renewal window has not opened")]
    RenewalWindowNotOpen,

    /// Block timestamp exceeds the `u64` range.
    #[error("consensus timestamp exceeds u64")]
    ConsensusTimestampExceedsU64,

    /// Stored integer field exceeds its canonical type width.
    #[error("{0} exceeds its canonical width")]
    FieldExceedsCanonicalWidth(&'static str),

    /// Codec or validation check failed with context.
    #[error("{context}: {reason}")]
    CodecRevert {
        /// Validation context label.
        context: &'static str,
        /// Underlying failure description.
        reason: String,
    },

    /// Registration call bypassed the ABI preflight check.
    #[error("V1 registration preflight was bypassed")]
    RegistrationPreflightBypassed,

    /// Measurement transition call bypassed the ABI preflight check.
    #[error("V1 measurement-transition preflight was bypassed")]
    MeasurementTransitionPreflightBypassed,

    /// Enclave upgrade call bypassed the ABI preflight check.
    #[error("upgrade preflight missing")]
    UpgradePreflightMissing,

    /// Node signature slice fails fixed-array conversion after preflight.
    #[error("node signature preflight")]
    NodeSignaturePreflight,

    /// Enclave signature slice fails fixed-array conversion after preflight.
    #[error("enclave signature preflight")]
    EnclaveSignaturePreflight,

    /// Active V1 policy fails canonical encoding.
    #[error("active V1 policy cannot be encoded: {0}")]
    ActiveV1PolicyEncodingFailed(String),

    /// Staged successor V1 policy fails canonical encoding.
    #[error("staged successor V1 policy cannot be encoded: {0}")]
    StagedSuccessorV1PolicyEncodingFailed(String),

    /// Registration binding argument bypassed the ABI preflight check.
    #[error("V1 registration binding preflight was bypassed")]
    RegistrationBindingPreflightBypassed,

    /// Validator NodeHost binding fails canonical decoding.
    #[error("validator NodeHost binding is not canonical: {0}")]
    ValidatorNodeHostBindingNotCanonical(String),

    /// Validator signature argument bypassed the ABI preflight check.
    #[error("V1 validator signature preflight was bypassed")]
    ValidatorSignaturePreflightBypassed,

    /// Validator signature slice fails fixed-array conversion after preflight.
    #[error("preflight validator signature mismatch")]
    PreflightValidatorSignatureMismatch,

    /// NodeHost binding signature argument bypassed the ABI preflight check.
    #[error("V1 NodeHost binding signature preflight was bypassed")]
    NodeHostBindingSignaturePreflightBypassed,

    /// NodeHost binding signature slice fails fixed-array conversion.
    #[error("preflight NodeHost binding signature mismatch")]
    PreflightNodeHostBindingSignatureMismatch,

    /// Renew or replace mutator bypassed the ABI preflight check.
    #[error("V1 {0} preflight was bypassed")]
    MutatorPreflightBypassed(&'static str),

    /// Registration intent contains an invalid node identity.
    #[error("registration node identity is invalid: {0}")]
    RegistrationNodeIdentityInvalid(String),

    /// Mutator gas calculation rejected the input dimensions.
    #[error("invalid V1 registry gas dimensions: {0}")]
    InvalidV1RegistryGasDimensions(String),

    /// Calldata intrinsic gas calculation rejected the input length.
    #[error("invalid V1 calldata gas dimensions: {0}")]
    InvalidV1CalldataGasDimensions(String),

    /// Protocol gas calculation underflows intrinsic gas.
    #[error("V1 protocol gas underflow")]
    ProtocolGasUnderflow,

    /// Storage gas allowance exceeds the total protocol gas.
    #[error("V1 mutator storage allowance exceeds fixed gas")]
    MutatorStorageAllowanceExceedsFixedGas,

    /// Prepaid protocol gas is smaller than the base precompile charge.
    #[error("V1 protocol gas is below the precompile base charge")]
    ProtocolGasBelowBaseCharge,

    /// Attestation evidence exceeds the protocol byte limit.
    #[error("attestation evidence exceeds {0} bytes")]
    AttestationEvidenceExceedsMaxBytes(usize),

    /// Node proof-of-possession signature is not 65 bytes.
    #[error("node proof-of-possession signature must be 65 bytes")]
    NodePopSignatureLengthInvalid,

    /// Enclave proof-of-possession signature is not 64 bytes.
    #[error("enclave proof-of-possession signature must be 64 bytes")]
    EnclavePopSignatureLengthInvalid,

    /// Validator NodeHost binding signature is not 65 bytes.
    #[error("validator NodeHost binding signature must be 65 bytes")]
    ValidatorBindingSignatureLengthInvalid,

    /// NodeHost binding signature is not 65 bytes.
    #[error("NodeHost binding signature must be 65 bytes")]
    NodeBindingSignatureLengthInvalid,

    /// Validator NodeHost binding payload has an invalid byte length.
    #[error("validator NodeHost binding must be {0} bytes")]
    ValidatorNodeBindingLengthInvalid(usize),

    /// Calldata framing around the evidence exceeds the byte limit.
    #[error("evidence call framing exceeds {0} bytes")]
    EvidenceCallFramingExceedsMaxBytes(usize),

    /// Preflight node signature slice has an invalid length.
    #[error("preflight node signature mismatch")]
    PreflightNodeSignatureMismatch,

    /// Preflight enclave signature slice has an invalid length.
    #[error("preflight enclave signature mismatch")]
    PreflightEnclaveSignatureMismatch,

    /// Calldata violates canonical V1 registration ABI framing rules.
    #[error("invalid canonical V1 registration ABI: {0}")]
    InvalidCanonicalRegistrationAbi(&'static str),

    /// Stored V1 binding hash does not match the requested identity hash.
    #[error("stored V1 binding identity mismatch")]
    StoredV1BindingIdentityMismatch,

    /// Created registration did not produce an onboarding artifact.
    #[error("created registration has no purpose-bound onboarding artifact")]
    MissingPurposeBoundOnboardingArtifact,

    /// Onboarding artifact context does not match the committed binding.
    #[error("purpose-bound onboarding artifact does not match committed Registry binding")]
    OnboardingArtifactBindingMismatch,

    /// Onboarding artifact fails canonical encoding.
    #[error("purpose-bound onboarding artifact is non-canonical: {code:#06x}")]
    OnboardingArtifactNonCanonical {
        /// Numeric error code from the canonical encoder.
        code: u16,
    },

    /// Upgrade candidate preparation lacks an approved MRENCLAVE upgrade.
    #[error("candidate requires an approved MRENCLAVE upgrade")]
    CandidateRequiresApprovedUpgrade,

    /// Pending upgrade candidate does not match the expected context hash.
    #[error("pending upgrade changed or is absent")]
    PendingUpgradeChangedOrAbsent,

    /// Measurement transition arrived at or after successor policy activation.
    #[error("measurement rollout closes at successor policy activation")]
    MeasurementRolloutClosed,

    /// Evidence attestation mode differs from the active policy mode.
    #[error("attestation evidence mode does not match the active V1 policy")]
    AttestationEvidenceModeMismatch,

    /// Direct-dev measurement transition lacks an active MRENCLAVE upgrade.
    #[error("DirectDev transition requires a MRENCLAVE upgrade")]
    DirectDevTransitionRequiresUpgrade,

    /// Measurement transition evidence omits the key-ready proof.
    #[error("measurement transition is missing key-ready proof")]
    MissingTransitionKeyReadyProof,

    /// Measurement transition occurred before the offer-key commitment.
    #[error("measurement transition requires the permanent offer-key commitment")]
    TransitionRequiresOfferKeyCommitment,

    /// Measurement transition key-ready proof fails verification.
    #[error("measurement transition key-ready proof is invalid: {0}")]
    InvalidTransitionKeyReadyProof(String),

    /// Enclave-resident DCAP verifier failed or is unavailable.
    #[error("enclave-resident DCAP verifier is unavailable or unauthenticated: {0}")]
    DcapVerifierUnavailable(String),

    /// Enclave-resident DCAP verifier rejected the evidence.
    #[error("DCAP evidence rejected with code {code:#06x}")]
    DcapEvidenceRejected {
        /// Numeric rejection code from the DCAP verifier.
        code: u16,
    },

    /// Accepted DCAP evidence fails hash calculation.
    #[error("accepted DCAP evidence cannot be hashed: {code:#06x}")]
    AcceptedDcapEvidenceHashingFailed {
        /// Numeric error code from the evidence hasher.
        code: u16,
    },

    /// Caller requested an onboarding artifact for a non-registration call.
    #[error("onboarding artifact requested for a non-registration operation")]
    OnboardingArtifactNonRegistration,

    /// DCAP registration occurred before the OST3 offer-key commitment.
    #[error("DcapRequired registration requires the OST3 offer-key commitment")]
    DcapRegistrationRequiresOfferKeyCommitment,

    /// Purpose-bound DCAP onboarding verifier failed or is unavailable.
    #[error("purpose-bound DCAP onboarding verifier is unavailable or unauthenticated: {0}")]
    DcapOnboardingVerifierUnavailable(String),

    /// Direct-dev evidence signature does not match the registration intent.
    #[error("GramineDirectDev evidence signature does not bind the registration intent")]
    DirectDevSignatureMismatch,

    /// Direct-dev registration occurred before the OST3 offer-key commitment.
    #[error("GramineDirectDev registration requires the OST3 offer-key commitment")]
    DirectDevRegistrationRequiresOfferKeyCommitment,

    /// Direct-dev onboarding artifact generation failed.
    #[error("purpose-bound GramineDirectDev onboarding artifact failed: {0}")]
    DirectDevOnboardingArtifactFailed(String),

    /// Upgrade candidate replay has a different evidence hash.
    #[error("candidate is not an exact evidence replay")]
    CandidateNotExactEvidenceReplay,

    /// Registry mutation replay has a different evidence hash.
    #[error("registry mutation is not an exact evidence replay")]
    MutationNotExactEvidenceReplay,

    /// Stored binding or enclave id conflicts with the replayed intent.
    #[error("stored V1 idempotency identity is inconsistent")]
    StoredV1IdempotencyIdentityInconsistent,

    /// Initial V1 policy is already present with different content.
    #[error("initial V1 policy is already installed")]
    InitialV1PolicyAlreadyInstalled,

    /// Initial V1 policy hash conflicts with the stored anchor.
    #[error("initial V1 policy hash conflicts with registry anchor")]
    InitialV1PolicyHashConflict,

    /// V1 policy chunk count overflows `u32`.
    #[error("V1 policy has too many chunks")]
    V1PolicyTooManyChunks,

    /// V1 policy byte length overflows `u32`.
    #[error("V1 policy is too large")]
    V1PolicyTooLarge,

    /// Successor policy stage request has a zero proposal id.
    #[error("successor policy proposal id must be nonzero")]
    SuccessorPolicyProposalIdZero,

    /// Another successor V1 policy is already staged in storage.
    #[error("another successor V1 policy is already staged")]
    AnotherSuccessorV1PolicyStaged,

    /// Successor V1 policy chunk count overflows `u32`.
    #[error("successor V1 policy has too many chunks")]
    SuccessorV1PolicyTooManyChunks,

    /// Successor V1 policy byte length overflows `u32`.
    #[error("successor V1 policy is too large")]
    SuccessorV1PolicyTooLarge,

    /// Empty staged V1 policy has non-zero anchor slots.
    #[error("empty staged V1 policy has non-empty anchors")]
    EmptyStagedV1PolicyNonEmptyAnchors,

    /// Stored staged V1 policy fails canonical decoding.
    #[error("stored staged V1 policy is non-canonical: {0}")]
    StoredStagedV1PolicyNonCanonical(String),

    /// Stored staged V1 policy fails hash calculation.
    #[error("stored staged V1 policy cannot be hashed: {0}")]
    StoredStagedV1PolicyHashingFailed(String),

    /// Stored staged V1 policy hash differs from its anchor slot.
    #[error("stored staged V1 policy hash does not match its anchor")]
    StoredStagedV1PolicyHashMismatch,

    /// Stored staged V1 policy activation height differs from its anchor slot.
    #[error("stored staged V1 policy activation does not match its anchor")]
    StoredStagedV1PolicyActivationMismatch,

    /// Stored staged V1 policy has a zero proposal id.
    #[error("stored staged V1 policy has zero proposal id")]
    StoredStagedV1PolicyZeroProposalId,

    /// Promotion request found no staged successor V1 policy.
    #[error("no successor V1 policy is staged for this update")]
    NoSuccessorV1PolicyStagedForUpdate,

    /// Staged successor V1 policy fails canonical encoding during promotion.
    #[error("staged successor V1 policy is invalid: {0}")]
    StagedSuccessorV1PolicyInvalid(String),

    /// Staged successor V1 policy fails hash calculation during promotion.
    #[error("staged successor V1 policy cannot be hashed: {0}")]
    StagedSuccessorV1PolicyHashingFailed(String),

    /// Successor V1 policy chunk index overflows `u32` during promotion.
    #[error("successor V1 policy chunk index overflow")]
    SuccessorV1PolicyChunkIndexOverflow,

    /// Successor V1 policy byte length overflows `u32` during promotion.
    #[error("successor V1 policy length overflow")]
    SuccessorV1PolicyLengthOverflow,

    /// Discard request proposal id does not match the staged policy owner.
    #[error("cannot discard another update's staged V1 policy")]
    CannotDiscardAnotherUpdateStagedPolicy,

    /// Stored active V1 policy fails canonical decoding.
    #[error("stored V1 policy is non-canonical: {0}")]
    StoredV1PolicyNonCanonical(String),

    /// Stored active V1 policy chain identity does not match the host chain.
    #[error("stored V1 policy chain identity mismatch")]
    StoredV1PolicyChainIdentityMismatch,

    /// Stored active V1 policy fails hash calculation.
    #[error("stored V1 policy cannot be hashed: {0}")]
    StoredV1PolicyHashingFailed(String),

    /// Stored active V1 policy hash differs from the registry anchor.
    #[error("stored V1 policy hash does not match registry anchor")]
    StoredV1PolicyHashMismatch,

    /// Active V1 policy activation height exceeds the current block number.
    #[error("no V1 TEE policy is active at this height")]
    NoV1PolicyActiveAtHeight,

    /// Stored active V1 policy length overflows `usize`.
    #[error("stored V1 policy length overflow")]
    StoredV1PolicyLengthOverflow,

    /// Active V1 policy length is zero.
    #[error("V1 TEE policy is not installed")]
    V1PolicyNotInstalled,

    /// Stored active V1 policy length exceeds `MAX_TEE_POLICY_BYTES`.
    #[error("stored V1 policy exceeds the protocol cap")]
    StoredV1PolicyExceedsCap,

    /// Stored active V1 policy chunk index overflows `u32`.
    #[error("stored V1 policy index overflow")]
    StoredV1PolicyIndexOverflow,

    /// Stored staged V1 policy length overflows `usize`.
    #[error("stored staged V1 policy length overflow")]
    StoredStagedV1PolicyLengthOverflow,

    /// Caller requested staged V1 policy bytes when length is zero.
    #[error("staged V1 policy bytes requested while empty")]
    StagedV1PolicyEmptyRead,

    /// Stored staged V1 policy length exceeds `MAX_TEE_POLICY_BYTES`.
    #[error("stored staged V1 policy exceeds the protocol cap")]
    StoredStagedV1PolicyExceedsCap,

    /// Stored staged V1 policy chunk index overflows `u32`.
    #[error("stored staged V1 policy index overflow")]
    StoredStagedV1PolicyIndexOverflow,

    /// Initial V1 policy chain identity does not match the host chain.
    #[error("initial V1 policy chain identity mismatch")]
    InitialV1PolicyChainIdentityMismatch,

    /// Initial V1 policy attestation mode is invalid for the chain id.
    #[error("initial V1 policy attestation mode is not allowed for this chain")]
    InitialV1PolicyAttestationModeDisallowed,

    /// Initial V1 policy does not start at version one and block one.
    #[error("initial V1 policy must be version one at block one")]
    InitialV1PolicyInvalidVersionOrHeight,

    /// V1 policy version increment overflows `u64` during staging.
    #[error("V1 policy version overflow")]
    V1PolicyVersionOverflow,

    /// Successor V1 policy activation height is not in the future.
    #[error("successor V1 policy activation must be in the future")]
    SuccessorV1PolicyActivationNotInFuture,

    /// Staged successor V1 policy belongs to a different proposal id.
    #[error("staged successor V1 policy belongs to another update")]
    StagedSuccessorV1PolicyWrongProposal,

    /// Current block height is below the successor policy activation height.
    #[error("successor V1 policy activation height has not been reached")]
    SuccessorV1PolicyActivationHeightNotReached,

    /// Staged successor V1 policy changes the attestation mode at promotion.
    #[error("staged successor V1 policy changes attestation mode")]
    StagedSuccessorV1PolicyChangesAttestationMode,

    /// Staged successor V1 policy lineage no longer matches the active policy.
    #[error("staged successor V1 policy no longer follows current policy")]
    StagedSuccessorV1PolicyBrokenLineage,

    /// V1 policy version increment overflows `u64` during promotion.
    #[error("V1 policy version overflow")]
    FatalV1PolicyVersionOverflow,

    /// Successor V1 policy chain identity differs from the active policy.
    #[error("successor V1 policy chain identity mismatch")]
    SuccessorV1PolicyChainIdentityMismatch,

    /// Successor V1 policy attempts to change the attestation mode.
    #[error("successor V1 policy cannot change attestation mode")]
    SuccessorV1PolicyCannotChangeAttestationMode,

    /// Successor V1 policy version does not equal the active version plus one.
    #[error("successor V1 policy version is not current plus one")]
    SuccessorV1PolicyInvalidVersion,

    /// Successor V1 policy predecessor hash differs from the active policy hash.
    #[error("successor V1 policy predecessor hash mismatch")]
    SuccessorV1PolicyPredecessorHashMismatch,

    /// Registration caller differs from the validator address in the binding.
    #[error("registration caller does not match the NodeHost EVM association")]
    RegistrationCallerAssociationMismatch,

    /// Registration caller is already associated with a different NodeHost.
    #[error("registration caller is already associated with another NodeHost")]
    RegistrationCallerAlreadyAssociated,

    /// Registration caller is not associated with the existing NodeHost.
    #[error("registration caller is not associated with the existing NodeHost")]
    RegistrationCallerNotAssociatedWithExistingNodeHost,

    /// Jailed validator attempted enclave rejoin before completing unjail.
    #[error("jailed validator must complete ordinary unjail before enclave rejoin")]
    JailedValidatorCannotRejoin,

    /// Mutator caller is not associated with the target NodeHost.
    #[error("TEE mutator caller is not associated with the target NodeHost")]
    MutatorCallerNotAssociated,

    /// Initial address association references a different NodeHost.
    #[error("initial address association must reference the same NodeHost as the registration")]
    InitialAssociationTargetMismatch,

    /// Validator NodeHost binding fails signature verification.
    #[error("validator NodeHost binding proof of possession is invalid")]
    ValidatorNodeBindingPopInvalid,

    /// Validator NodeHost binding references an unregistered NodeHost.
    #[error("validator NodeHost binding references an unregistered NodeHost")]
    ValidatorNodeBindingUnregisteredNodeHost,

    /// Validator NodeHost binding references an expired NodeHost lease.
    #[error("validator NodeHost binding references an expired NodeHost")]
    ValidatorNodeBindingExpiredNodeHost,

    /// Validator NodeHost binding references a retired enclave measurement.
    #[error("validator NodeHost binding references a retired enclave")]
    ValidatorNodeBindingRetiredEnclave,

    /// Validator address is already bound to a different NodeHost.
    #[error("validator is already bound to another NodeHost")]
    ValidatorAlreadyBoundToAnotherNodeHost,

    /// Registration outcome and association outcome violate atomicity rules.
    #[error("NodeHost registration and address association outcomes are inconsistent")]
    InconsistentRegistrationAndAssociationOutcomes,

    /// Attestation intent operation or mode differs from the mutator context.
    #[error("attestation intent operation or mode does not match the registry mutator")]
    IntentOperationOrModeMismatch,

    /// Registration intent does not bind the admitted V1 policy hash.
    #[error("registration intent does not bind the authoritative V1 policy")]
    IntentPolicyHashMismatch,

    /// Registration enclave id does not match the key derivation output.
    #[error("registration enclave id is not derived from its persistent keys")]
    EnclaveIdDerivationMismatch,

    /// Node proof-of-possession signature fails verification.
    #[error("node proof of possession is invalid")]
    NodePopInvalid,

    /// Enclave proof-of-possession signature fails verification.
    #[error("enclave proof of possession is invalid")]
    EnclavePopInvalid,

    /// Enclave claims do not match a unique active measurement rule.
    #[error("verified enclave claims must match exactly one active profile measurement rule")]
    ClaimsMeasurementRuleMatchInvalid,

    /// Platform TCB status requires an advisory policy that is not enabled.
    #[error("QVL Platform TCB status is stricter than active policy allows")]
    PlatformTcbStatusDisallowed,

    /// Node already has a live upgrade candidate.
    #[error("cancel the live candidate before preparing another")]
    LiveCandidateAlreadyExists,

    /// Upgrade binding references a cancelled or consumed candidate.
    #[error("prepared upgrade binding was cancelled or consumed")]
    PreparedUpgradeBindingCancelledOrConsumed,

    /// Measurement transition does not match the staged upgrade candidate.
    #[error("transition does not match the live upgrade candidate")]
    TransitionDoesNotMatchLiveCandidate,

    /// Upgrade candidate preparation reached the active-binding emission path.
    #[error("candidate crossed active-binding write boundary")]
    CandidateCrossedActiveBindingWriteBoundary,

    /// Binding policy change attempted without measurement transition.
    #[error(
        "changing an existing binding's policy requires transitionEnclaveMeasurement and its resident-key proof"
    )]
    PolicyChangeRequiresMeasurementTransition,

    /// Registration attempted on a binding whose lease is still active.
    #[error("live enclave binding must renew instead of rejoin")]
    LiveBindingMustRenew,

    /// Expired rejoin modifies the persistent NodeHost authorization hash.
    #[error("expired rejoin changes the persistent NodeHost authorization")]
    ExpiredRejoinChangesAuthorization,

    /// Expired rejoin does not increment the binding and registration versions.
    #[error("expired rejoin does not carry the exact next registration versions")]
    ExpiredRejoinVersionMismatch,

    /// Initial registration does not use canonical starting counters.
    #[error("initial registration versions and nonces are not canonical")]
    InitialRegistrationCountersNonCanonical,

    /// Renewal attempted for a node without an existing binding.
    #[error("cannot renew a missing enclave binding")]
    CannotRenewMissingBinding,

    /// Renewal does not increment the registration version and renewal nonce.
    #[error("renewal does not carry the exact next renewal version and nonce")]
    RenewalVersionOrNonceMismatch,

    /// Replacement attempted for a node without an existing binding.
    #[error("cannot replace a missing enclave binding")]
    CannotReplaceMissingBinding,

    /// Replacement modifies the persistent NodeHost authorization hash.
    #[error("replacement changes the persistent NodeHost authorization")]
    ReplacementChangesAuthorization,

    /// Replacement reuses the current enclave id or binding id.
    #[error("replacement must use a fresh enclave and binding id")]
    ReplacementRequiresFreshIds,

    /// Replacement does not carry the next binding and registration versions.
    #[error("replacement does not carry the exact next binding version")]
    ReplacementVersionMismatch,

    /// Measurement transition attempted for a node without an existing binding.
    #[error("cannot transition a missing enclave binding")]
    CannotTransitionMissingBinding,

    /// Measurement transition modifies the persistent NodeHost authorization.
    #[error("measurement transition changes the persistent NodeHost authorization")]
    TransitionChangesAuthorization,

    /// Measurement transition reuses the current enclave id or binding id.
    #[error("measurement transition must use a fresh enclave and binding id")]
    TransitionRequiresFreshIds,

    /// Measurement transition does not carry the expected versions and nonces.
    #[error("measurement transition does not carry the exact next versions and nonce")]
    TransitionCountersMismatch,

    /// Renewal binding was absent during lease validation.
    #[error("renewal binding disappeared during validation")]
    RenewalBindingDisappeared,

    /// Renewal lease deadline addition overflows `u64`.
    #[error("renewal deadline overflows u64")]
    RenewalDeadlineOverflow,

    /// Renewal requested deadline does not equal current deadline plus maximum lease.
    #[error("renewal must extend exactly one lease period from the current deadline")]
    RenewalMustExtendOneLeasePeriod,

    /// Requested lease expiration timestamp is not in the future.
    #[error("requested lease is already expired")]
    RequestedLeaseExpired,

    /// Requested lease duration falls outside the policy minimum or maximum.
    #[error("requested lease is outside active policy bounds")]
    RequestedLeaseOutsideBounds,

    /// Collateral expiration leaves less time than the required safety margin.
    #[error("verified collateral leaves no mandatory safety margin")]
    CollateralMarginExhausted,

    /// Requested lease expiration exceeds the collateral safety limit.
    #[error("requested lease exceeds verified collateral validity")]
    RequestedLeaseExceedsCollateralValidity,

    /// Enclave id is already registered to a different node.
    #[error("enclave is already bound to another node")]
    EnclaveAlreadyBoundToAnotherNode,

    /// Binding id was already used by a different node.
    #[error("binding id has already been used by another node")]
    BindingIdAlreadyUsedByAnotherNode,

    /// Stored reverse ownership mappings do not match the renewing node.
    #[error("stored V1 renewal reverse ownership is inconsistent")]
    StoredRenewalReverseOwnershipInconsistent,

    /// Successor enclave id or binding id was already used.
    #[error("successor enclave or binding id has already been used")]
    SuccessorIdsAlreadyUsed,

    /// Stored reverse ownership mappings do not match the rejoining node.
    #[error("expired rejoin found inconsistent current reverse ownership")]
    ExpiredRejoinReverseOwnershipInconsistent,

    /// Rejoin enclave id belongs to another node.
    #[error("expired rejoin enclave is not current or globally fresh")]
    ExpiredRejoinEnclaveNotAvailable,

    /// Rejoin binding id was already used.
    #[error("expired rejoin binding id has already been used")]
    ExpiredRejoinBindingIdAlreadyUsed,
}

impl TeeRegistryError {
    /// Returns true when the error represents a fatal invariant violation.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::VerifiedDcapVerdictEncodingFailed { .. }
                | Self::InvalidActiveV1LeasePeriod
                | Self::FieldExceedsCanonicalWidth(_)
                | Self::RegistrationPreflightBypassed
                | Self::MeasurementTransitionPreflightBypassed
                | Self::UpgradePreflightMissing
                | Self::NodeSignaturePreflight
                | Self::EnclaveSignaturePreflight
                | Self::ActiveV1PolicyEncodingFailed(_)
                | Self::StagedSuccessorV1PolicyEncodingFailed(_)
                | Self::RegistrationBindingPreflightBypassed
                | Self::ValidatorSignaturePreflightBypassed
                | Self::PreflightValidatorSignatureMismatch
                | Self::NodeHostBindingSignaturePreflightBypassed
                | Self::PreflightNodeHostBindingSignatureMismatch
                | Self::MutatorPreflightBypassed(_)
                | Self::ProtocolGasUnderflow
                | Self::MutatorStorageAllowanceExceedsFixedGas
                | Self::ProtocolGasBelowBaseCharge
                | Self::PreflightNodeSignatureMismatch
                | Self::PreflightEnclaveSignatureMismatch
                | Self::StoredV1BindingIdentityMismatch
                | Self::MissingPurposeBoundOnboardingArtifact
                | Self::OnboardingArtifactBindingMismatch
                | Self::OnboardingArtifactNonCanonical { .. }
                | Self::TransitionRequiresOfferKeyCommitment
                | Self::DcapVerifierUnavailable(_)
                | Self::AcceptedDcapEvidenceHashingFailed { .. }
                | Self::OnboardingArtifactNonRegistration
                | Self::DcapRegistrationRequiresOfferKeyCommitment
                | Self::DcapOnboardingVerifierUnavailable(_)
                | Self::DirectDevRegistrationRequiresOfferKeyCommitment
                | Self::DirectDevOnboardingArtifactFailed(_)
                | Self::StoredV1IdempotencyIdentityInconsistent
                | Self::EmptyStagedV1PolicyNonEmptyAnchors
                | Self::StoredStagedV1PolicyNonCanonical(_)
                | Self::StoredStagedV1PolicyHashingFailed(_)
                | Self::StoredStagedV1PolicyHashMismatch
                | Self::StoredStagedV1PolicyActivationMismatch
                | Self::StoredStagedV1PolicyZeroProposalId
                | Self::StagedSuccessorV1PolicyInvalid(_)
                | Self::StagedSuccessorV1PolicyHashingFailed(_)
                | Self::SuccessorV1PolicyChunkIndexOverflow
                | Self::SuccessorV1PolicyLengthOverflow
                | Self::StoredV1PolicyNonCanonical(_)
                | Self::StoredV1PolicyChainIdentityMismatch
                | Self::StoredV1PolicyHashingFailed(_)
                | Self::StoredV1PolicyHashMismatch
                | Self::StoredV1PolicyLengthOverflow
                | Self::StoredV1PolicyExceedsCap
                | Self::StoredV1PolicyIndexOverflow
                | Self::StoredStagedV1PolicyLengthOverflow
                | Self::StagedV1PolicyEmptyRead
                | Self::StoredStagedV1PolicyExceedsCap
                | Self::StoredStagedV1PolicyIndexOverflow
                | Self::StagedSuccessorV1PolicyChangesAttestationMode
                | Self::StagedSuccessorV1PolicyBrokenLineage
                | Self::FatalV1PolicyVersionOverflow
                | Self::InconsistentRegistrationAndAssociationOutcomes
                | Self::CandidateCrossedActiveBindingWriteBoundary
                | Self::RenewalBindingDisappeared
                | Self::StoredRenewalReverseOwnershipInconsistent
                | Self::ExpiredRejoinReverseOwnershipInconsistent
        )
    }

    /// Builds a codec revert error with a static context prefix.
    #[must_use]
    pub fn codec_revert(context: &'static str, error: impl std::fmt::Display) -> PrecompileError {
        Self::CodecRevert {
            context,
            reason: error.to_string(),
        }
        .into()
    }

    /// Maps an attestation evidence decode failure into a precompile error.
    #[must_use]
    pub fn attestation_evidence_not_canonical(error: impl std::fmt::Display) -> PrecompileError {
        Self::AttestationEvidenceNotCanonical(error.to_string()).into()
    }

    /// Maps a TEE policy hash failure into a precompile error.
    #[must_use]
    pub fn invalid_tee_policy(error: impl std::fmt::Display) -> PrecompileError {
        Self::InvalidTeePolicy(error.to_string()).into()
    }

    /// Maps an active V1 policy encoding failure into a fatal precompile error.
    #[must_use]
    pub fn active_v1_policy_encoding_failed(error: impl std::fmt::Display) -> PrecompileError {
        Self::ActiveV1PolicyEncodingFailed(error.to_string()).into()
    }

    /// Maps a staged successor V1 policy encoding failure into a fatal error.
    #[must_use]
    pub fn staged_successor_v1_policy_encoding_failed(
        error: impl std::fmt::Display,
    ) -> PrecompileError {
        Self::StagedSuccessorV1PolicyEncodingFailed(error.to_string()).into()
    }

    /// Maps a validator NodeHost binding decode failure into a revert error.
    #[must_use]
    pub fn validator_node_host_binding_not_canonical(
        error: impl std::fmt::Display,
    ) -> PrecompileError {
        Self::ValidatorNodeHostBindingNotCanonical(error.to_string()).into()
    }

    /// Maps a registration node identity failure into a revert error.
    #[must_use]
    pub fn registration_node_identity_invalid(error: impl std::fmt::Display) -> PrecompileError {
        Self::RegistrationNodeIdentityInvalid(error.to_string()).into()
    }

    /// Maps a registry gas dimension failure into a revert error.
    #[must_use]
    pub fn invalid_v1_registry_gas_dimensions(error: impl std::fmt::Display) -> PrecompileError {
        Self::InvalidV1RegistryGasDimensions(error.to_string()).into()
    }

    /// Maps a calldata gas dimension failure into a revert error.
    #[must_use]
    pub fn invalid_v1_calldata_gas_dimensions(error: impl std::fmt::Display) -> PrecompileError {
        Self::InvalidV1CalldataGasDimensions(error.to_string()).into()
    }

    /// Maps a transition key-ready proof failure into a revert error.
    #[must_use]
    pub fn invalid_transition_key_ready_proof(error: impl std::fmt::Display) -> PrecompileError {
        Self::InvalidTransitionKeyReadyProof(error.to_string()).into()
    }

    /// Maps a DCAP verifier transport failure into a fatal precompile error.
    #[must_use]
    pub fn dcap_verifier_unavailable(error: impl std::fmt::Display) -> PrecompileError {
        Self::DcapVerifierUnavailable(error.to_string()).into()
    }

    /// Maps a DCAP onboarding verifier failure into a fatal precompile error.
    #[must_use]
    pub fn dcap_onboarding_verifier_unavailable(error: impl std::fmt::Display) -> PrecompileError {
        Self::DcapOnboardingVerifierUnavailable(error.to_string()).into()
    }

    /// Maps a direct-dev onboarding artifact failure into a fatal error.
    #[must_use]
    pub fn direct_dev_onboarding_artifact_failed(error: impl std::fmt::Display) -> PrecompileError {
        Self::DirectDevOnboardingArtifactFailed(error.to_string()).into()
    }

    /// Maps a stored staged V1 policy decode failure into a fatal error.
    #[must_use]
    pub fn stored_staged_v1_policy_non_canonical(error: impl std::fmt::Display) -> PrecompileError {
        Self::StoredStagedV1PolicyNonCanonical(error.to_string()).into()
    }

    /// Maps a stored staged V1 policy hash failure into a fatal error.
    #[must_use]
    pub fn stored_staged_v1_policy_hashing_failed(
        error: impl std::fmt::Display,
    ) -> PrecompileError {
        Self::StoredStagedV1PolicyHashingFailed(error.to_string()).into()
    }

    /// Maps a staged successor V1 policy encoding failure during promotion.
    #[must_use]
    pub fn staged_successor_v1_policy_invalid(error: impl std::fmt::Display) -> PrecompileError {
        Self::StagedSuccessorV1PolicyInvalid(error.to_string()).into()
    }

    /// Maps a staged successor V1 policy hash failure during promotion.
    #[must_use]
    pub fn staged_successor_v1_policy_hashing_failed(
        error: impl std::fmt::Display,
    ) -> PrecompileError {
        Self::StagedSuccessorV1PolicyHashingFailed(error.to_string()).into()
    }

    /// Maps a stored active V1 policy decode failure into a fatal error.
    #[must_use]
    pub fn stored_v1_policy_non_canonical(error: impl std::fmt::Display) -> PrecompileError {
        Self::StoredV1PolicyNonCanonical(error.to_string()).into()
    }

    /// Maps a stored active V1 policy hash failure into a fatal error.
    #[must_use]
    pub fn stored_v1_policy_hashing_failed(error: impl std::fmt::Display) -> PrecompileError {
        Self::StoredV1PolicyHashingFailed(error.to_string()).into()
    }
}

impl From<TeeRegistryError> for PrecompileError {
    fn from(error: TeeRegistryError) -> Self {
        if error.is_fatal() {
            Self::Fatal(error.to_string())
        } else {
            Self::Revert(error.to_string())
        }
    }
}

/// Extracts the deterministic revert message from `error` in unit tests.
#[cfg(test)]
pub(crate) fn extract_revert_message(error: PrecompileError) -> String {
    match error {
        PrecompileError::Revert(message) => message,
        other => panic!("expected deterministic revert, got {other:?}"),
    }
}

/// Returns true when `error` is a fatal precompile error in unit tests.
#[cfg(test)]
pub(crate) fn is_fatal_error(error: &PrecompileError) -> bool {
    matches!(error, PrecompileError::Fatal(_))
}

/// Returns true when `error` is a fatal precompile error that contains `needle`.
#[cfg(test)]
pub(crate) fn is_fatal_containing(error: &PrecompileError, needle: &str) -> bool {
    matches!(error, PrecompileError::Fatal(message) if message.contains(needle))
}

/// Formats a displayable error into a string for test callbacks.
#[cfg(test)]
pub(crate) fn format_test_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
