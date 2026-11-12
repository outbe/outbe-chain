//! Defines error types for OST3 TEE bootstrap and startup DKG coordination.

use alloy_primitives::Address;
use thiserror::Error;

/// Represents errors from TEE bootstrap message encoding, validation, and ceremony coordination.
#[derive(Debug, Error)]
pub enum TeeBootstrapError {
    /// Indicates that a message payload exceeds the maximum delivery size.
    #[error("OST3 delivery payload exceeds protocol maximum")]
    DeliveryPayloadExceedsMax,
    /// Indicates that the bounded bootstrap delivery queue is full.
    #[error("OST3 bounded delivery queue is full")]
    DeliveryQueueFull,
    /// Indicates that the bootstrap gossip channel closed before all peers acknowledged.
    #[error("OST3 bootstrap gossip closed before all committee peers acknowledged delivery")]
    BootstrapGossipClosedBeforeAck,
    /// Indicates that the bootstrap gossip timed out waiting for peer acknowledgements.
    #[error("OST3 bootstrap gossip timed out waiting for committee acknowledgements")]
    BootstrapGossipTimedOutWaitingForAck,
    /// Indicates that the DKG gossip channel closed during the ceremony.
    #[error("TEE DKG gossip channel closed mid-ceremony")]
    DkgGossipClosedMidCeremony,
    /// Indicates that the DKG gossip timed out waiting for a ceremony message.
    #[error("TEE DKG ceremony recv timed out after ~90s")]
    DkgCeremonyRecvTimedOut,
    /// Indicates that a validator equivocated between two valid OST3 submissions.
    #[error("validator {0} equivocated between two valid OST3 submissions")]
    ValidatorEquivocatedSubmissions(Address),
    /// Indicates that the OST3 committee size is outside protocol bounds.
    #[error("OST3 committee size is outside protocol bounds")]
    CommitteeSizeOutOfBounds,
    /// Indicates that the local OST3 submission validator and EVM signer differ.
    #[error("OST3 local submission and EVM signer differ")]
    LocalSubmissionAndEvmSignerDiffer,
    /// Indicates that assembling the unsigned OST3 payload failed.
    #[error("OST3 unsigned assembly failed: {0}")]
    UnsignedAssemblyFailed(String),
    /// Indicates that computing the OST3 signing hash failed.
    #[error("OST3 signing hash failed: {0}")]
    SigningHashFailed(String),
    /// Indicates that signing the OST3 payload with the local EVM signer failed.
    #[error("OST3 local signature failed: {0}")]
    LocalSignatureFailed(String),
    /// Indicates that the collected OST3 signature set is incomplete.
    #[error("OST3 signature set is incomplete")]
    SignatureSetIncomplete,
    /// Indicates that the signed OST3 payload failed preflight checks.
    #[error("signed OST3 payload failed preflight: {0}")]
    SignedPayloadFailedPreflight(String),
    /// Indicates that broadcasting the local OST3 submission failed.
    #[error("OST3 submission broadcast failed: {0}")]
    SubmissionBroadcastFailed(String),
    /// Indicates that the OST3 gossip channel closed before all submissions arrived.
    #[error("OST3 gossip closed before all submissions arrived")]
    GossipClosedBeforeAllSubmissions,
    /// Indicates that a validator equivocated between two OST3 signatures.
    #[error("validator {0} equivocated between OST3 signatures")]
    ValidatorEquivocatedSignatures(Address),
    /// Indicates that broadcasting the local OST3 signature failed.
    #[error("OST3 signature broadcast failed: {0}")]
    SignatureBroadcastFailed(String),
    /// Indicates that the OST3 gossip channel closed before all signatures arrived.
    #[error("OST3 gossip closed before all signatures arrived")]
    GossipClosedBeforeAllSignatures,
    /// Indicates that the identity gossip channel closed before all identities arrived.
    #[error("TEE DKG identity gossip closed before all {0} identities collected")]
    IdentityGossipClosedBeforeAllCollected(usize),
    /// Indicates that the identity exchange timed out before collecting all peers.
    #[error("TEE DKG identity exchange timed out: collected {collected}/{expected}")]
    IdentityExchangeTimedOut {
        /// Number of identities collected.
        collected: usize,
        /// Expected number of identities.
        expected: usize,
    },
    /// Indicates that the early signed identity buffer exceeded its participant budget.
    #[error("TEE DKG early signed identity budget exceeded: {0}")]
    EarlySignedIdentityBudgetExceeded(usize),
    /// Indicates that encoding the OST3 attestation evidence failed.
    #[error("cannot encode OST3 evidence: {0}")]
    EncodeEvidenceFailed(String),
    /// Indicates that the encoded OST3 evidence length exceeds u32::MAX.
    #[error("OST3 evidence length exceeds u32")]
    EvidenceLengthExceedsU32,
    /// Indicates that encoding the OST3 validator binding failed.
    #[error("cannot encode OST3 binding: {0}")]
    EncodeBindingFailed(String),
    /// Indicates that the OST3 startup policy is invalid.
    #[error("invalid OST3 startup policy: {0}")]
    InvalidStartupPolicy(String),
    /// Indicates that the enclave GetPublicKeys request failed during DKG startup.
    #[error("TEE DKG GetPublicKeys failed: {0}")]
    DkgGetPublicKeysFailed(String),
    /// Indicates that the enclave returned an unexpected response to GetPublicKeys.
    #[error("unexpected GetPublicKeys response: {0}")]
    UnexpectedGetPublicKeysResponse(String),
    /// Indicates that the startup TEE DKG ceremony failed.
    #[error("TEE DKG ceremony failed: {0}")]
    DkgCeremonyFailed(String),
    /// Indicates that the enclave GetPublicKeys request failed when querying the offer key.
    #[error("TEE GetPublicKeys failed: {0}")]
    GetPublicKeysFailed(String),
    /// Indicates that the local enclave permanent offer key is not ready.
    #[error("local enclave permanent offer key is not ready; no recovery or fallback exists")]
    PermanentOfferKeyNotReady,
    /// Indicates that the local enclave reported a ready but zero permanent offer key.
    #[error("local enclave reports a ready but zero permanent offer key")]
    PermanentOfferKeyZero,
    /// Indicates that the TEE DKG participant set is invalid.
    #[error("invalid TEE DKG participant set: {0}")]
    InvalidDkgParticipantSet(String),
    /// Indicates that the TEE DKG ceremony binding is invalid.
    #[error("invalid TEE DKG ceremony binding: {0}")]
    InvalidDkgCeremonyBinding(String),
    /// Indicates that the enclave DKG announcement request failed.
    #[error("TEE DKG announcement failed: {0}")]
    DkgAnnouncementFailed(String),
    /// Indicates that the enclave returned an unexpected DKG announcement response.
    #[error("unexpected DKG announcement response: {0}")]
    UnexpectedDkgAnnouncementResponse(String),
    /// Indicates that the OST3 policy is invalid.
    #[error("invalid OST3 policy: {0}")]
    InvalidPolicy(String),
    /// Indicates that the OST3 registration intent is invalid.
    #[error("invalid OST3 registration intent: {0}")]
    InvalidRegistrationIntent(String),
    /// Indicates that signing the OST3 registration intent with the NodeHost key failed.
    #[error("cannot sign OST3 registration intent: {0}")]
    SignRegistrationIntentFailed(String),
    /// Indicates that the local OST3 evidence failed canonical encoding.
    #[error("local OST3 evidence is not canonical: {0}")]
    LocalEvidenceNotCanonical(String),
    /// Indicates that a DcapRequired genesis policy cannot use a development enclave session.
    #[error("DcapRequired genesis policy cannot use a development enclave session")]
    DcapRequiredCannotUseDevelopmentSession,
    /// Indicates that the OST3 node identity is invalid.
    #[error("invalid OST3 node identity: {0}")]
    InvalidNodeIdentity(String),
    /// Indicates that production OST3 lacks a committed NodeHost manifest.
    #[error("production OST3 requires one committed NodeHost manifest")]
    ProductionRequiresCommittedManifest,
    /// Indicates that the committed NodeHost manifest mismatches the chain or P2P identity.
    #[error("committed NodeHost manifest does not match OST3 chain or persistent P2P identity")]
    CommittedManifestMismatch,
    /// Indicates that the committed enclave identity is invalid.
    #[error("invalid committed enclave identity: {0}")]
    InvalidCommittedEnclaveIdentity(String),
    /// Indicates that the committed NodeHost authorization is invalid.
    #[error("invalid committed NodeHost authorization: {0}")]
    InvalidCommittedNodeHostAuthorization(String),
    /// Indicates that the GramineDirectDev session lacks its enclave identity quote.
    #[error("GramineDirectDev session did not retain its enclave identity quote")]
    DevSessionMissingIdentityQuote,
    /// Indicates that the validator NodeHost binding is invalid.
    #[error("invalid validator NodeHost binding: {0}")]
    InvalidValidatorNodeHostBinding(String),
    /// Indicates that signing the validator NodeHost binding with the EVM key failed.
    #[error("cannot sign validator NodeHost binding: {0}")]
    SignValidatorNodeHostBindingFailed(String),
    /// Indicates that signing the NodeHost validator binding with the NodeHost key failed.
    #[error("cannot sign NodeHost validator binding: {0}")]
    SignNodeHostValidatorBindingFailed(String),
    /// Indicates that generating the production OST3 DCAP quote failed.
    #[error("production OST3 quote generation failed: {0}")]
    ProductionQuoteGenerationFailed(String),
    /// Indicates that acquiring the production OST3 DCAP collateral failed.
    #[error("production OST3 collateral acquisition failed: {0}")]
    ProductionCollateralAcquisitionFailed(String),
    /// Indicates that signing the dev registration intent in a production session failed.
    #[error("production SGX-no-attest OST3 intent signing failed: {0}")]
    ProductionDevIntentSigningFailed(String),
    /// Indicates that signing the dev registration intent in a development session failed.
    #[error("development OST3 intent signing failed: {0}")]
    DevelopmentIntentSigningFailed(String),
    /// Indicates that the OST3 NodeHost identity is invalid during validation.
    #[error("invalid OST3 NodeHost identity: {0}")]
    InvalidNodeHostIdentity(String),
    /// Indicates that an OST3 submission does not prove a canonical committee registration.
    #[error("OST3 submission does not prove one canonical committee registration")]
    SubmissionNotCanonicalCommitteeRegistration,
    /// Indicates that the GramineDirectDev evidence does not match the outer enclave proof.
    #[error("OST3 GramineDirectDev evidence does not match the outer enclave proof")]
    DevEvidenceMismatchOuterProof,
    /// Indicates that signing a prehash with the NodeHost secp256k1 key failed.
    #[error("{0}")]
    NodeHostSignPrehash(String),
    /// Indicates that the recipient mode is unsupported for tracked delivery.
    #[error("unsupported recipient mode for tracked delivery")]
    UnsupportedRecipientModeForTrackedDelivery,
    /// Indicates that the pending delivery budget was exceeded.
    #[error("pending delivery budget exceeded: messages={messages}, bytes={bytes}")]
    PendingDeliveryBudgetExceeded {
        /// Pending message count.
        messages: usize,
        /// Pending byte count.
        bytes: usize,
    },
}

impl TeeBootstrapError {
    /// Formats a NodeHost prehash signing error message.
    pub fn node_host_sign_prehash(error: impl std::fmt::Display) -> Self {
        Self::NodeHostSignPrehash(error.to_string())
    }

    /// Formats an unexpected `GetPublicKeys` enclave response.
    pub fn unexpected_get_public_keys_response(response: impl std::fmt::Debug) -> Self {
        Self::UnexpectedGetPublicKeysResponse(format!("{response:?}"))
    }

    /// Formats an unexpected `DkgParticipantAnnounceV1` enclave response.
    pub fn unexpected_dkg_announcement_response(response: impl std::fmt::Debug) -> Self {
        Self::UnexpectedDkgAnnouncementResponse(format!("{response:?}"))
    }
}
