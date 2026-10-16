//! Typed errors for TEE lease renewal and journal checks.

use alloy_primitives::{Address, U256};
use outbe_primitives::tee_attestation_v1::CodecError;
use outbe_tee::{dcap_protocol::DcapRejectCodeV1, TransportError};
use thiserror::Error;

/// Errors from the manual TEE renewal reducer.
#[derive(Debug, Error)]
pub enum RenewalError {
    /// The renewal enclave does not sign `GramineDirectDev` intents.
    #[error("renewal enclave does not support GramineDirectDev intent signing")]
    DirectDevSigningUnsupported,

    /// The hasher rejected the manifest node identity.
    #[error("hash manifest node identity: {0}")]
    HashManifestNodeIdentity(CodecError),

    /// The hasher failed to derive the manifest enclave id.
    #[error("derive manifest enclave identity: {0}")]
    DeriveManifestEnclaveIdentity(CodecError),

    /// The hasher failed to derive the manifest authorization hash.
    #[error("derive manifest NodeHost authorization: {0}")]
    DeriveManifestAuthorization(CodecError),

    /// The hasher rejected the finalized policy.
    #[error("hash finalized policy: {0}")]
    HashFinalizedPolicy(CodecError),

    /// The local manifest disagrees with the finalized Registry binding.
    #[error("committed NodeHost manifest does not match the finalized Registry binding")]
    ManifestBindingMismatch,

    /// The renewal selector disagrees with the local node identity.
    #[error("renewal selector does not match the committed node identity")]
    SelectorMismatch,

    /// The finalized binding matches neither the source nor the target.
    #[error("finalized Registry binding matches neither renewal source nor target")]
    BindingNeitherSourceNorTarget,

    /// The decoder rejected the journaled renewal intent.
    #[error("decode journal renewal intent: {0}")]
    DecodeJournalIntent(CodecError),

    /// The local manifest disagrees with the promoted enclave.
    #[error("renewal manifest does not match the promoted enclave")]
    PromotedManifestMismatch,

    /// An active upgrade journal blocks concurrent renewal.
    #[error(
        "renewal is blocked while an enclave upgrade is journaled to preserve exact transition counters"
    )]
    BlockedByActiveUpgrade,

    /// The finalized binding diverged from the renewal journal.
    #[error("finalized Registry binding diverged from the renewal journal")]
    FinalizedBindingDiverged,

    /// The Registry lease period is zero or odd.
    #[error("finalized Registry lease period is not a positive even duration")]
    InvalidLeasePeriod,

    /// The finalized enclave lease already expired.
    #[error("finalized enclave lease expired; run tee join to recover")]
    LeaseExpired,

    /// The maximum lease duration overflowed `u64`.
    #[error("maximum renewal lease overflows timestamp")]
    MaximumLeaseOverflow,

    /// The hasher rejected the renewal intent.
    #[error("hash renewal intent: {0}")]
    HashRenewalIntent(CodecError),

    /// The gas schedule rejected the renewal call parameters.
    #[error("calculate normative renewal gas: {0}")]
    CalculateNormativeGas(CodecError),

    /// The EVM signer balance cannot cover the gas cost.
    #[error("renewal EVM signer {address} has {balance} but needs at least {required_balance}")]
    InsufficientSignerBalance {
        address: Address,
        balance: U256,
        required_balance: U256,
    },

    /// The encoder rejected the renewal intent.
    #[error("encode canonical renewal intent: {0}")]
    EncodeRenewalIntent(CodecError),

    /// The hasher rejected the active policy.
    #[error("hash active policy: {0}")]
    HashActivePolicy(CodecError),

    /// The registration version counter overflowed `u64`.
    #[error("registration version exhausted")]
    RegistrationVersionExhausted,

    /// The renewal nonce counter overflowed `u64`.
    #[error("renewal nonce exhausted")]
    RenewalNonceExhausted,

    /// The verifier rejected the signed DCAP collateral window.
    #[error("validate signed renewal collateral window: {0:?}")]
    ValidateSignedCollateralWindow(DcapRejectCodeV1),

    /// The collateral margin exceeds the expiration ceiling.
    #[error("renewal collateral cannot satisfy the active margin")]
    CollateralMarginUnderflow,

    /// The fresh Intel collateral does not cover the next deadline.
    #[error("fresh Intel collateral cannot cover the exact next renewal deadline")]
    CollateralWindowDoesNotCoverDeadline,

    /// The encoder rejected the renewal evidence.
    #[error("encode canonical renewal evidence: {0}")]
    EncodeRenewalEvidence(CodecError),

    /// The hasher rejected the DCAP renewal evidence.
    #[error("hash canonical renewal DCAP evidence: {0:?}")]
    HashDcapRenewalEvidence(DcapRejectCodeV1),

    /// The verifier rejected the `GramineDirectDev` enclave signature.
    #[error("GramineDirectDev enclave signature does not bind renewal intent")]
    DirectDevSignatureMismatch,

    /// The hasher rejected the renewal evidence.
    #[error("hash canonical renewal evidence: {0}")]
    HashRenewalEvidence(CodecError),

    /// The collateral provider failed to fetch renewal collateral.
    #[error("acquire renewal collateral: {0}")]
    AcquireCollateral(TransportError),

    /// The verifier rejected the acquired DCAP collateral.
    #[error("validate renewal collateral: {0:?}")]
    ValidateCollateral(DcapRejectCodeV1),

    /// The prepared attempt contains no relay transaction.
    #[error("renewal attempt has no relay transaction")]
    MissingRelayTransaction,

    /// The RPC server returned a different transaction hash.
    #[error("RPC returned a transaction hash different from the signed renewal bytes")]
    SubmittedHashMismatch,

    /// The receipt omits the `transactionHash` field.
    #[error("exact renewal transaction receipt has no transactionHash")]
    MissingReceiptTransactionHash,

    /// The receipt hash disagrees with the expected hash.
    #[error("renewal RPC returned a receipt for a different transaction hash")]
    ReceiptTransactionHashMismatch,

    /// The receipt omits the `status` field.
    #[error("exact renewal transaction receipt has no status")]
    MissingReceiptStatus,

    /// The receipt status is not `0x1`.
    #[error("exact renewal transaction receipt has non-success status {0}")]
    NonSuccessReceiptStatus(String),

    /// Test mock fault or invariant violation.
    #[cfg(test)]
    #[error("{0}")]
    TestFault(&'static str),
}

/// Errors from TEE renewal journal files and checks.
#[derive(Debug, Error)]
pub enum RenewalJournalError {
    /// The snapshot has an unsupported version or zero generation.
    #[error("unsupported renewal journal version or generation")]
    UnsupportedVersionOrGeneration,

    /// Another process holds the renewal journal lock.
    #[error("another renewal process owns the journal lock")]
    JournalLocked,

    /// The journal generation counter overflowed `u64`.
    #[error("renewal journal generation exhausted")]
    GenerationExhausted,

    /// The renewal directory path has no parent directory.
    #[error("renewal directory has no parent")]
    DirectoryHasNoParent,

    /// The renewal directory violates owner-only `0700` permissions.
    #[error("renewal journal directory is not owner-only 0700")]
    InvalidDirectoryPermissions,

    /// The renewal file violates owner-only `0600` permissions.
    #[error("renewal journal file is not owner-only 0600")]
    InvalidFilePermissions,

    /// The renewal file metadata exceeds the size limit.
    #[error("renewal journal file exceeds its size cap")]
    FileSizeCapExceeded,

    /// The renewal journal payload exceeds the size limit.
    #[error("renewal journal exceeds its size cap")]
    JournalSizeCapExceeded,

    /// The attempt has empty fields or invalid array lengths.
    #[error("renewal journal contains invalid bounded material")]
    InvalidBoundedMaterial,

    /// The decoder rejected the journaled intent.
    #[error("decode renewal journal intent: {0}")]
    DecodeIntent(CodecError),

    /// The hasher rejected the journaled intent.
    #[error("hash renewal journal intent: {0}")]
    HashIntent(CodecError),

    /// The node signature slice does not have 65 bytes.
    #[error("renewal journal node signature length changed")]
    InvalidNodeSignatureLength,

    /// The enclave signature slice does not have 64 bytes.
    #[error("renewal journal enclave signature length changed")]
    InvalidEnclaveSignatureLength,

    /// The source registration version counter overflowed `u64`.
    #[error("renewal journal source registration version exhausted")]
    SourceRegistrationVersionExhausted,

    /// The source renewal nonce counter overflowed `u64`.
    #[error("renewal journal source nonce exhausted")]
    SourceNonceExhausted,

    /// The hasher rejected the journaled NodeHost identity.
    #[error("hash renewal journal NodeHost identity: {0}")]
    HashNodeHostIdentity(CodecError),

    /// The hasher failed to derive the journaled enclave id.
    #[error("derive renewal journal enclave identity: {0}")]
    DeriveEnclaveIdentity(CodecError),

    /// The journaled intent does not match the next transition.
    #[error("renewal journal intent is not the exact next binding transition")]
    NotExactNextBindingTransition,

    /// The decoder rejected the journaled evidence.
    #[error("decode renewal journal evidence: {0}")]
    DecodeEvidence(CodecError),

    /// The evidence variant disagrees with the intent mode.
    #[error("renewal journal evidence variant does not match intent mode")]
    EvidenceModeMismatch,

    /// The hasher rejected the journaled DCAP evidence.
    #[error("hash renewal journal DCAP evidence: {0:?}")]
    HashDcapEvidence(DcapRejectCodeV1),

    /// The journaled `GramineDirectDev` evidence failed signature checks.
    #[error("renewal journal contains invalid GramineDirectDev evidence")]
    InvalidDirectDevEvidence,

    /// The hasher rejected the journaled evidence.
    #[error("hash renewal journal evidence: {0}")]
    HashEvidence(CodecError),

    /// A journaled hash disagrees with its encoded payload.
    #[error("renewal journal hash commitment mismatch")]
    HashCommitmentMismatch,

    /// The calldata is not the canonical `renewEnclave` call.
    #[error("renewal journal calldata is not the canonical renewal call")]
    NonCanonicalCalldata,

    /// The first relay variant disagrees with the attempt relay.
    #[error("renewal journal relay binding mismatch")]
    RelayBindingMismatch,

    /// A relay variant has inconsistent fields or hashes.
    #[error("renewal journal contains a competing relay variant")]
    CompetingRelayVariant,

    /// The collateral margin exceeds the collateral deadline.
    #[error("renewal collateral margin underflow")]
    CollateralMarginUnderflow,

    /// The requested lease exceeds the collateral ceiling.
    #[error("renewal journal lease exceeds collateral ceiling")]
    LeaseExceedsCollateralCeiling,

    /// `GramineDirectDev` mode requires sentinel collateral values.
    #[error("renewal journal has non-canonical GramineDirectDev collateral fields")]
    NonCanonicalDirectDevCollateral,

    /// The submitted hash list disagrees with the relay variants.
    #[error("submitted renewal journal transaction list is non-canonical")]
    NonCanonicalSubmittedTransactionList,

    /// The finalized binding disagrees with the attempt hashes.
    #[error("finalized renewal journal binding mismatch")]
    FinalizedBindingMismatch,

    /// The abandoned state has an empty or long reason.
    #[error("abandoned renewal journal reason is invalid")]
    InvalidAbandonedReason,
}
