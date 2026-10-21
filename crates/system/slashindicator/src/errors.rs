//! Domain error types for the slash indicator precompile.

use alloy_primitives::{Address, B256};
use outbe_consensus::proof::V2VerifyError;
use outbe_primitives::error::{PrecompileError, Result};
use thiserror::Error;

/// Domain errors for the slash indicator precompile.
#[derive(Debug, Error)]
pub enum SlashIndicatorError {
    /// Two evidence blocks have different signers.
    #[error("evidence blocks must have the same signer")]
    EvidenceSignerMismatch,

    /// Two proposals in double-proposal evidence target different rounds.
    #[error("proposals must be for the same round")]
    ProposalsMustBeSameRound,

    /// Two votes in equivocation evidence target different rounds.
    #[error("votes must be for the same round")]
    VotesMustBeSameRound,

    /// Two proposals in double-proposal evidence are identical.
    #[error("proposals must differ for double-proposal evidence")]
    ProposalsMustDifferForDoubleProposal,

    /// Two conflicting votes target the same proposal.
    #[error("conflicting votes must be for different proposals")]
    ConflictingVotesMustBeDifferentProposals,

    /// Conflicting vote evidence lacks one valid notarize and one valid nullify signature.
    #[error("evidence must contain one valid notarize and one valid nullify signature")]
    ConflictingVoteSignaturesInvalid,

    /// The precompile already processed this evidence hash.
    #[error("evidence already processed")]
    EvidenceAlreadyProcessed,

    /// Raw evidence block is shorter than 145 bytes.
    #[error("evidence block too short: need at least 145 bytes")]
    EvidenceBlockTooShort,

    /// Public key slice conversion failed.
    #[error("evidence pubkey slice")]
    EvidencePubkeySlice,

    /// Signature slice conversion failed.
    #[error("evidence signature slice")]
    EvidenceSignatureSlice,

    /// Evidence block contains an invalid BLS public key.
    #[error("invalid BLS public key in evidence")]
    InvalidBlsPublicKey,

    /// Evidence block contains an invalid BLS signature.
    #[error("invalid BLS signature in evidence")]
    InvalidBlsSignature,

    /// Varint reader reached the end of the buffer early.
    #[error("unexpected end of data while reading varint")]
    UnexpectedEndOfVarint,

    /// Varint shift reached or exceeded 64 bits.
    #[error("varint overflow")]
    VarintOverflow,

    /// Varint uses a redundant encoding.
    #[error("noncanonical varint")]
    NonCanonicalVarint,

    /// Effective evidence reward percentage exceeds the burned amount.
    #[error("invalid evidence reward percent: {percent}")]
    InvalidEvidenceRewardPercent {
        /// Configured effective percentage.
        percent: u64,
    },

    /// Evidence reward calculation exceeded U256.
    #[error("evidence reward overflow")]
    EvidenceRewardOverflow,

    /// The slash guard ring cursor overflowed `u64`.
    #[error("slash_guard_ring_seq overflow")]
    SlashGuardRingSeqOverflow,

    /// The evidence submitter is not an active validator.
    #[error("submitter {caller} is not an ACTIVE validator (status: {status})")]
    SubmitterNotActiveValidator {
        /// Caller address.
        caller: Address,
        /// Stored validator status code.
        status: u8,
    },

    /// No committee snapshot exists for the evidence epoch.
    #[error(
        "no committee snapshot for evidence epoch {epoch}; cannot verify the committee-bound vote signature"
    )]
    MissingCommitteeSnapshotForEpoch {
        /// Consensus epoch number.
        epoch: u64,
    },

    /// A committee snapshot contains an invalid BLS public key.
    #[error("invalid committee pubkey in snapshot")]
    InvalidCommitteePubkeyInSnapshot,

    /// The evidence signer is not a registered validator.
    #[error("signer is not a registered validator")]
    SignerNotRegisteredValidator,

    /// A fixed-size wire field is truncated.
    #[error("evidence field is truncated")]
    EvidenceFieldTruncated,

    /// Seed-partial equivocation evidence has an invalid byte length.
    #[error("SPE1 evidence must be exactly {expected} bytes, got {actual}")]
    Spe1WrongLength {
        /// Expected byte length.
        expected: usize,
        /// Actual byte length.
        actual: usize,
    },

    /// Seed-partial equivocation evidence has invalid magic bytes.
    #[error("SPE1 evidence bad magic")]
    Spe1BadMagic,

    /// Seed-partial equivocation evidence has an unsupported version byte.
    #[error("SPE1 evidence bad version")]
    Spe1BadVersion,

    /// Invalid-seed-partial evidence is shorter than the fixed prefix.
    #[error("IPE1 evidence too short: need at least {min_len} bytes, got {actual}")]
    Ipe1TooShort {
        /// Minimum required byte length.
        min_len: usize,
        /// Actual byte length.
        actual: usize,
    },

    /// Invalid-seed-partial evidence has invalid magic bytes.
    #[error("IPE1 evidence bad magic")]
    Ipe1BadMagic,

    /// Invalid-seed-partial evidence has an unsupported version byte.
    #[error("IPE1 evidence bad version")]
    Ipe1BadVersion,

    /// Invalid-seed-partial commitment length exceeds the input buffer.
    #[error("IPE1 commitment length exceeds input")]
    Ipe1CommitmentLengthExceedsInput,

    /// Invalid-seed-partial evidence has trailing bytes after the commitment.
    #[error("IPE1 evidence has trailing bytes")]
    Ipe1TrailingBytes,

    /// Invalid-VRF-proof evidence is shorter than the fixed prefix.
    #[error("InvalidVrfProofEvidence: input too short ({actual} < {min_len} bytes)")]
    InvalidVrfEvidenceInputTooShort {
        /// Actual byte length.
        actual: usize,
        /// Minimum required byte length.
        min_len: usize,
    },

    /// Invalid-VRF-proof evidence has invalid magic bytes.
    #[error("InvalidVrfProofEvidence: bad magic")]
    InvalidVrfEvidenceBadMagic,

    /// Invalid-VRF-proof evidence has an unsupported version byte.
    #[error("InvalidVrfProofEvidence: unsupported version {version} (expected {expected})")]
    InvalidVrfEvidenceUnsupportedVersion {
        /// Decoded version byte.
        version: u8,
        /// Expected version byte.
        expected: u8,
    },

    /// Invalid-VRF-proof evidence has trailing bytes after the transaction payload.
    #[error("InvalidVrfProofEvidence: {trailing} trailing bytes after phase1_tx")]
    InvalidVrfEvidenceTrailingBytes {
        /// Count of trailing bytes.
        trailing: usize,
    },

    /// A length-prefixed field in invalid-VRF-proof evidence exceeds the remaining input.
    #[error(
        "InvalidVrfProofEvidence: {field}_bytes length {len} exceeds remaining input ({remaining} bytes left)"
    )]
    InvalidVrfEvidenceFieldExceedsInput {
        /// Field name prefix.
        field: &'static str,
        /// Declared byte length.
        len: usize,
        /// Remaining bytes in the input slice.
        remaining: usize,
    },

    /// The accused child block is older than the maximum age window.
    #[error("evidence stale: current_block {current_block} > child_block {child_block} + max_age {max_age}")]
    EvidenceStale {
        /// Current block number.
        current_block: u64,
        /// Accused child block number.
        child_block: u64,
        /// Maximum allowed block age.
        max_age: u64,
    },

    /// The parent block number does not precede the child block number by one.
    #[error("evidence parent/child number mismatch: parent={parent} child={child}")]
    EvidenceParentChildNumberMismatch {
        /// Parent block number.
        parent: u64,
        /// Child block number.
        child: u64,
    },

    /// The referenced block number is outside the canonical history window.
    #[error("evidence {role} block {number} not in canonical-history window")]
    EvidenceBlockNotInCanonicalWindow {
        /// Block role label.
        role: &'static str,
        /// Block number.
        number: u64,
    },

    /// The referenced block hash does not match the canonical chain hash.
    #[error(
        "evidence {role} hash {hash} is not canonical at block {number} (canonical: {canonical})"
    )]
    EvidenceHashNotCanonical {
        /// Block role label.
        role: &'static str,
        /// Provided block hash.
        hash: B256,
        /// Block number.
        number: u64,
        /// Canonical block hash at that number.
        canonical: B256,
    },

    /// Recovery of the Phase 1 proposer failed.
    #[error("phase1_tx invalid: {reason}")]
    Phase1TxInvalid {
        /// Underlying recovery error text.
        reason: String,
    },

    /// Decoding of the Phase 1 system transaction calldata failed.
    #[error("phase1 calldata decode failed: {reason}")]
    Phase1CalldataDecodeFailed {
        /// Underlying decode error text.
        reason: String,
    },

    /// The Phase 1 calldata is not a certified parent accounting payload.
    #[error("phase1 calldata is not CertifiedParentAccounting: {kind}")]
    Phase1CalldataNotCertifiedParentAccounting {
        /// Debug representation of the decoded system transaction kind.
        kind: String,
    },

    /// No committee snapshot exists for the finalized epoch and committee hash.
    #[error(
        "no committee snapshot for finalized_epoch={finalized_epoch} committee_set_hash={committee_set_hash}"
    )]
    MissingCommitteeSnapshotForFinalizedEpoch {
        /// Finalized epoch number.
        finalized_epoch: u64,
        /// Committee set hash.
        committee_set_hash: B256,
    },

    /// The recovered Phase 1 proposer is not in the epoch committee.
    #[error("phase1_tx proposer {proposer} not in committee for epoch {epoch}")]
    Phase1ProposerNotInCommittee {
        /// Recovered proposer address.
        proposer: Address,
        /// Finalized epoch number.
        epoch: u64,
    },

    /// The two VRF seed partials in equivocation evidence are identical.
    #[error("not equivocation: the two partials are identical")]
    IdenticalSeedPartials,

    /// One or both VRF seed partials lack a valid identity signature.
    #[error("both partials must carry a valid identity signature from the accused signer")]
    InvalidSeedPartialIdentitySignatures,

    /// No committee snapshot exists for the round epoch and committee hash.
    #[error("no committee snapshot for (round_epoch, committee_set_hash)")]
    MissingCommitteeSnapshotForRound,

    /// The evidence payload exceeds the maximum allowed byte size.
    #[error("evidence too large: {actual} > {max_bytes} bytes")]
    EvidenceTooLarge {
        /// Actual byte length.
        actual: usize,
        /// Maximum allowed byte length.
        max_bytes: usize,
    },

    /// The evidence epoch is older than the maximum epoch lag.
    #[error(
        "evidence epoch-stale: current_epoch {current_epoch} > {label} {evidence_epoch} + max_lag {max_lag}"
    )]
    EvidenceEpochStale {
        /// Current on-chain epoch.
        current_epoch: u64,
        /// Epoch field label.
        label: &'static str,
        /// Evidence epoch value.
        evidence_epoch: u64,
        /// Maximum allowed epoch lag.
        max_lag: u64,
    },

    /// The Phase 1 metadata parent block binding does not match the evidence parent block.
    #[error(
        "metadata parent binding mismatch: evidence=({ev_parent_num}, {ev_parent_hash}), metadata=({meta_num}, {meta_hash})"
    )]
    MetadataParentBindingMismatch {
        /// Evidence parent block number.
        ev_parent_num: u64,
        /// Evidence parent block hash.
        ev_parent_hash: B256,
        /// Metadata finalized block number.
        meta_num: u64,
        /// Metadata finalized block hash.
        meta_hash: B256,
    },

    /// The Phase 1 metadata epoch does not match the evidence child epoch.
    #[error("metadata epoch {finalized_epoch} does not match evidence child_epoch {child_epoch}")]
    MetadataEpochMismatch {
        /// Metadata finalized epoch.
        finalized_epoch: u64,
        /// Evidence child epoch.
        child_epoch: u64,
    },

    /// The submitted proof is valid and cannot trigger slashing.
    #[error("evidence shows a VALID proof; nothing to slash")]
    ValidVrfProofNotSlashable,

    /// Proof verification failed with a non-VRF error class.
    #[error("verify_v2_proof rejected with non-VRF class ({err}); not slashable here")]
    NonVrfVerifyRejection {
        /// Underlying V2 verification error.
        err: V2VerifyError,
    },

    /// The signer index is outside the committee range.
    #[error("signer index out of committee range")]
    SignerIndexOutOfRange,

    /// The signer public key does not match the committee entry at the given index.
    #[error("signer pubkey does not match committee entry at index")]
    SignerPubkeyMismatchAtIndex,

    /// The VRF material version does not match the committee snapshot.
    #[error("vrf material version does not match committee snapshot")]
    VrfMaterialVersionMismatch,

    /// The committee snapshot has a zero polynomial commitment hash.
    #[error("committee snapshot has no polynomial commitment")]
    MissingSnapshotPolynomialCommitment,

    /// The carried commitment hash does not match the committee snapshot polynomial hash.
    #[error("commitment does not match committee snapshot polynomial hash")]
    CommitmentPolynomialHashMismatch,

    /// The VRF seed partial lacks a valid identity signature from the accused signer.
    #[error("partial is not identity-signed by the accused signer")]
    PartialNotIdentitySigned,

    /// The polynomial commitment or VRF seed partial is malformed.
    #[error("malformed commitment or partial")]
    MalformedCommitmentOrPartial,

    /// The VRF seed partial is valid and cannot trigger slashing.
    #[error("seed partial is valid; nothing to slash")]
    ValidSeedPartialNotSlashable,
}

impl SlashIndicatorError {
    /// Builds a revert error for an invalid Phase 1 transaction.
    pub fn phase1_tx_invalid(err: impl std::fmt::Display) -> PrecompileError {
        Self::Phase1TxInvalid {
            reason: err.to_string(),
        }
        .into()
    }

    /// Builds a revert error when Phase 1 calldata decoding fails.
    pub fn phase1_calldata_decode_failed(err: impl std::fmt::Display) -> PrecompileError {
        Self::Phase1CalldataDecodeFailed {
            reason: err.to_string(),
        }
        .into()
    }
}

impl From<SlashIndicatorError> for PrecompileError {
    fn from(err: SlashIndicatorError) -> Self {
        Self::Revert(err.to_string())
    }
}

/// Extracts the revert reason string from a precompile result when it reverts.
pub fn revert_reason<T>(result: Result<T>) -> Option<String> {
    match result {
        Err(PrecompileError::Revert(reason)) => Some(reason),
        _ => None,
    }
}
