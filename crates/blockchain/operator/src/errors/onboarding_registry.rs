//! Typed errors for finalized TEE onboarding and Registry queries.

use outbe_primitives::tee_attestation_v1::CodecError;
use thiserror::Error;

/// Errors from finalized TEE onboarding checks.
#[derive(Debug, Error)]
pub enum OnboardingError {
    /// The finality poll exceeded its time limit.
    #[error("timed out after {0}s waiting for exact finalized TeeRegistry onboarding")]
    Timeout(u64),

    /// The `registerEnclave` transaction reverted in the block.
    #[error("registerEnclave transaction {transaction_hash} reverted in block {block_number}")]
    TransactionReverted {
        transaction_hash: String,
        block_number: u64,
    },

    /// A chain reorg replaced the receipt block before finality.
    #[error(
        "registerEnclave receipt was reorged before finality; key possession is possible but canonical join is not claimed"
    )]
    ReceiptReorged,

    /// The finalized Registry binding disagrees with the submitted transaction.
    #[error("finalized Registry binding does not match the submitted registration")]
    BindingMismatch,

    /// The receipt omits the `transactionHash` field.
    #[error("registration receipt has no transactionHash")]
    MissingReceiptTransactionHash,

    /// The receipt hash disagrees with the requested hash.
    #[error("registration receipt transaction hash mismatch")]
    ReceiptTransactionHashMismatch,

    /// The receipt status is not 0 or 1.
    #[error("registration receipt has non-canonical status {0}")]
    NonCanonicalReceiptStatus(u64),

    /// The event log omits the `transactionHash` field.
    #[error("onboarding log has no transactionHash")]
    MissingLogTransactionHash,

    /// The event log omits the `topics` array.
    #[error("onboarding log has no topics")]
    MissingLogTopics,

    /// The event topics do not match the expected values.
    #[error("onboarding event topics are non-canonical")]
    NonCanonicalEventTopics,

    /// The event log omits the `data` field.
    #[error("onboarding log has no data")]
    MissingLogData,

    /// The decoder rejected the onboarding artifact.
    #[error("invalid onboarding artifact: {0:#06x}")]
    InvalidArtifact(u16),

    /// The artifact context disagrees with the expected binding.
    #[error("onboarding artifact context does not match the exact registration")]
    ArtifactContextMismatch,

    /// The transaction emitted more than one matching artifact.
    #[error("transaction emitted more than one exact onboarding artifact")]
    DuplicateArtifact,

    /// The finalized transaction emitted no matching artifact.
    #[error("finalized transaction has no exact onboarding artifact")]
    MissingArtifact,

    /// The event data has a non-canonical offset word.
    #[error("onboarding event has non-canonical bytes offset")]
    NonCanonicalBytesOffset,

    /// The artifact length exceeds the protocol limit.
    #[error("onboarding artifact exceeds its protocol cap")]
    ArtifactExceedsCap,

    /// The event length overflowed `usize`.
    #[error("onboarding event length overflow")]
    EventLengthOverflow,

    /// The event payload or padding is non-canonical.
    #[error("onboarding event data or padding is non-canonical")]
    NonCanonicalEventDataOrPadding,

    /// A JSON object omits a required field.
    #[error("{context} has no {field}")]
    MissingField {
        context: &'static str,
        field: &'static str,
    },
}

/// Errors from finalized Registry policy and binding reads.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// The finalized Registry has no binding for this node.
    #[error("finalized Registry has no enclave binding for this node")]
    MissingEnclaveBinding,

    /// The finalized block JSON omits a required field.
    #[error("finalized block has no {0}")]
    FinalizedBlockMissingField(&'static str),

    /// An empty staged policy view contains non-empty anchors.
    #[error("empty finalized staged policy view has non-empty anchors")]
    EmptyStagedPolicyNonEmptyAnchors,

    /// The decoder or hasher rejected the active successor policy.
    #[error("invalid active successor: {0}")]
    InvalidActiveSuccessor(CodecError),

    /// The active upgrade anchors disagree with the active policy.
    #[error("activated enclave upgrade anchors disagree")]
    ActivatedUpgradeAnchorsDisagree,

    /// The staged policy view omits the proposal id or bytes.
    #[error("finalized staged policy view is incomplete")]
    IncompleteStagedPolicyView,

    /// The decoder rejected the finalized staged policy.
    #[error("finalized staged TEE policy is non-canonical: {0}")]
    NonCanonicalStagedPolicy(CodecError),

    /// The staged policy chain id disagrees with `eth_chainId`.
    #[error("finalized staged TEE policy chain id does not match eth_chainId")]
    StagedPolicyChainIdMismatch,

    /// The hasher rejected the active policy.
    #[error("invalid policy: {0}")]
    InvalidPolicy(CodecError),

    /// The binding and staged policy use different finalized heads.
    #[error("renewal binding and staged policy use different finalized heads")]
    RenewalStagedHeadMismatch,

    /// The hasher rejected the staged policy.
    #[error("invalid staged policy: {0}")]
    InvalidStagedPolicy(CodecError),

    /// The network does not use DCAP mode.
    #[error("DCAP renewal is disabled for non-DcapRequired networks")]
    DcapRenewalDisabled,

    /// The renewal schedule disagrees with the finalized block.
    #[error("finalized schedule and eth finalized block disagree")]
    FinalizedScheduleDisagreesWithBlock,

    /// The decoder rejected the finalized active policy.
    #[error("finalized TEE policy is non-canonical: {0}")]
    NonCanonicalPolicy(CodecError),

    /// The active policy chain id disagrees with `eth_chainId`.
    #[error("finalized TEE policy chain id does not match eth_chainId")]
    PolicyChainIdMismatch,

    /// The finalized Registry returned a zero offer key.
    #[error("finalized Registry tribute offer public key is zero")]
    ZeroTributeOfferPublicKey,

    /// An unexpected Registry selector was called in tests.
    #[cfg(test)]
    #[error("unexpected Registry call")]
    UnexpectedRegistryCall,
}
