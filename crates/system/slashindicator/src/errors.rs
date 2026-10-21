//! Domain error types for the slash indicator precompile.

use crate::precompile::ISlashIndicator;
use alloy_primitives::Bytes;
use alloy_primitives::{Address, B256};
use alloy_sol_types::SolError;
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
    fn from(error: SlashIndicatorError) -> Self {
        match error {
            SlashIndicatorError::EvidenceSignerMismatch => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::EvidenceSignerMismatch {}.abi_encode()),
            ),
            SlashIndicatorError::ProposalsMustBeSameRound => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::ProposalsMustBeSameRound {}.abi_encode()),
            ),
            SlashIndicatorError::VotesMustBeSameRound => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::VotesMustBeSameRound {}.abi_encode(),
            )),
            SlashIndicatorError::ProposalsMustDifferForDoubleProposal => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::ProposalsMustDifferForDoubleProposal {}.abi_encode(),
                ))
            }
            SlashIndicatorError::ConflictingVotesMustBeDifferentProposals => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::ConflictingVotesMustBeDifferentProposals {}.abi_encode(),
                ))
            }
            SlashIndicatorError::ConflictingVoteSignaturesInvalid => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::ConflictingVoteSignaturesInvalid {}.abi_encode()),
            ),
            SlashIndicatorError::EvidenceAlreadyProcessed => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::EvidenceAlreadyProcessed {}.abi_encode()),
            ),
            SlashIndicatorError::EvidenceBlockTooShort => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::EvidenceBlockTooShort {}.abi_encode()),
            ),
            SlashIndicatorError::EvidencePubkeySlice => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::EvidencePubkeySlice {}.abi_encode(),
            )),
            SlashIndicatorError::EvidenceSignatureSlice => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::EvidenceSignatureSlice {}.abi_encode()),
            ),
            SlashIndicatorError::InvalidBlsPublicKey => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::InvalidBlsPublicKey {}.abi_encode(),
            )),
            SlashIndicatorError::InvalidBlsSignature => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::InvalidBlsSignature {}.abi_encode(),
            )),
            SlashIndicatorError::UnexpectedEndOfVarint => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::UnexpectedEndOfVarint {}.abi_encode()),
            ),
            SlashIndicatorError::VarintOverflow => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::VarintOverflow {}.abi_encode(),
            )),
            SlashIndicatorError::SlashGuardRingSeqOverflow => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::SlashGuardRingSeqOverflow {}.abi_encode()),
            ),
            SlashIndicatorError::SubmitterNotActiveValidator { caller, status } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::SubmitterNotActiveValidator { caller, status }.abi_encode(),
                ))
            }
            SlashIndicatorError::MissingCommitteeSnapshotForEpoch { epoch } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::MissingCommitteeSnapshotForEpoch { epoch }.abi_encode(),
                ))
            }
            SlashIndicatorError::InvalidCommitteePubkeyInSnapshot => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::InvalidCommitteePubkeyInSnapshot {}.abi_encode()),
            ),
            SlashIndicatorError::SignerNotRegisteredValidator => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::SignerNotRegisteredValidator {}.abi_encode()),
            ),
            SlashIndicatorError::EvidenceFieldTruncated => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::EvidenceFieldTruncated {}.abi_encode()),
            ),
            SlashIndicatorError::Spe1WrongLength { expected, actual } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::Spe1WrongLength {
                        expected: expected as u64,
                        actual: actual as u64,
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::Spe1BadMagic => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::Spe1BadMagic {}.abi_encode(),
            )),
            SlashIndicatorError::Spe1BadVersion => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::Spe1BadVersion {}.abi_encode(),
            )),
            SlashIndicatorError::Ipe1TooShort { min_len, actual } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::Ipe1TooShort {
                        min_len: min_len as u64,
                        actual: actual as u64,
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::Ipe1BadMagic => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::Ipe1BadMagic {}.abi_encode(),
            )),
            SlashIndicatorError::Ipe1BadVersion => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::Ipe1BadVersion {}.abi_encode(),
            )),
            SlashIndicatorError::Ipe1CommitmentLengthExceedsInput => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::Ipe1CommitmentLengthExceedsInput {}.abi_encode()),
            ),
            SlashIndicatorError::Ipe1TrailingBytes => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::Ipe1TrailingBytes {}.abi_encode(),
            )),
            SlashIndicatorError::InvalidVrfEvidenceInputTooShort { actual, min_len } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::InvalidVrfEvidenceInputTooShort {
                        actual: actual as u64,
                        min_len: min_len as u64,
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::InvalidVrfEvidenceBadMagic => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::InvalidVrfEvidenceBadMagic {}.abi_encode()),
            ),
            SlashIndicatorError::InvalidVrfEvidenceUnsupportedVersion { version, expected } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::InvalidVrfEvidenceUnsupportedVersion { version, expected }
                        .abi_encode(),
                ))
            }
            SlashIndicatorError::InvalidVrfEvidenceTrailingBytes { trailing } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::InvalidVrfEvidenceTrailingBytes {
                        trailing: trailing as u64,
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::InvalidVrfEvidenceFieldExceedsInput {
                field,
                len,
                remaining,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::InvalidVrfEvidenceFieldExceedsInput {
                    field: field.to_string(),
                    len: len as u64,
                    remaining: remaining as u64,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::EvidenceStale {
                current_block,
                child_block,
                max_age,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::EvidenceStale {
                    current_block,
                    child_block,
                    max_age,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::EvidenceParentChildNumberMismatch { parent, child } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::EvidenceParentChildNumberMismatch { parent, child }
                        .abi_encode(),
                ))
            }
            SlashIndicatorError::EvidenceBlockNotInCanonicalWindow { role, number } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::EvidenceBlockNotInCanonicalWindow {
                        role: role.to_string(),
                        number,
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::EvidenceHashNotCanonical {
                role,
                hash,
                number,
                canonical,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::EvidenceHashNotCanonical {
                    role: role.to_string(),
                    hash,
                    number,
                    canonical,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::Phase1TxInvalid { reason } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::Phase1TxInvalid {
                        reason: reason.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::Phase1CalldataDecodeFailed { reason } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::Phase1CalldataDecodeFailed {
                        reason: reason.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::Phase1CalldataNotCertifiedParentAccounting { kind } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::Phase1CalldataNotCertifiedParentAccounting {
                        kind: kind.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::MissingCommitteeSnapshotForFinalizedEpoch {
                finalized_epoch,
                committee_set_hash,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::MissingCommitteeSnapshotForFinalizedEpoch {
                    finalized_epoch,
                    committee_set_hash,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::Phase1ProposerNotInCommittee { proposer, epoch } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::Phase1ProposerNotInCommittee { proposer, epoch }.abi_encode(),
                ))
            }
            SlashIndicatorError::IdenticalSeedPartials => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::IdenticalSeedPartials {}.abi_encode()),
            ),
            SlashIndicatorError::InvalidSeedPartialIdentitySignatures => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::InvalidSeedPartialIdentitySignatures {}.abi_encode(),
                ))
            }
            SlashIndicatorError::MissingCommitteeSnapshotForRound => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::MissingCommitteeSnapshotForRound {}.abi_encode()),
            ),
            SlashIndicatorError::EvidenceTooLarge { actual, max_bytes } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::EvidenceTooLarge {
                        actual: actual as u64,
                        max_bytes: max_bytes as u64,
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::EvidenceEpochStale {
                current_epoch,
                label,
                evidence_epoch,
                max_lag,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::EvidenceEpochStale {
                    current_epoch,
                    label: label.to_string(),
                    evidence_epoch,
                    max_lag,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::MetadataParentBindingMismatch {
                ev_parent_num,
                ev_parent_hash,
                meta_num,
                meta_hash,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::MetadataParentBindingMismatch {
                    ev_parent_num,
                    ev_parent_hash,
                    meta_num,
                    meta_hash,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::MetadataEpochMismatch {
                finalized_epoch,
                child_epoch,
            } => PrecompileError::RevertBytes(Bytes::from(
                ISlashIndicator::MetadataEpochMismatch {
                    finalized_epoch,
                    child_epoch,
                }
                .abi_encode(),
            )),
            SlashIndicatorError::ValidVrfProofNotSlashable => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::ValidVrfProofNotSlashable {}.abi_encode()),
            ),
            SlashIndicatorError::NonVrfVerifyRejection { err } => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::NonVrfVerifyRejection {
                        err: err.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            SlashIndicatorError::SignerIndexOutOfRange => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::SignerIndexOutOfRange {}.abi_encode()),
            ),
            SlashIndicatorError::SignerPubkeyMismatchAtIndex => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::SignerPubkeyMismatchAtIndex {}.abi_encode()),
            ),
            SlashIndicatorError::VrfMaterialVersionMismatch => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::VrfMaterialVersionMismatch {}.abi_encode()),
            ),
            SlashIndicatorError::MissingSnapshotPolynomialCommitment => {
                PrecompileError::RevertBytes(Bytes::from(
                    ISlashIndicator::MissingSnapshotPolynomialCommitment {}.abi_encode(),
                ))
            }
            SlashIndicatorError::CommitmentPolynomialHashMismatch => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::CommitmentPolynomialHashMismatch {}.abi_encode()),
            ),
            SlashIndicatorError::PartialNotIdentitySigned => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::PartialNotIdentitySigned {}.abi_encode()),
            ),
            SlashIndicatorError::MalformedCommitmentOrPartial => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::MalformedCommitmentOrPartial {}.abi_encode()),
            ),
            SlashIndicatorError::ValidSeedPartialNotSlashable => PrecompileError::RevertBytes(
                Bytes::from(ISlashIndicator::ValidSeedPartialNotSlashable {}.abi_encode()),
            ),
        }
    }
}

/// Extracts the revert reason string from a precompile result when it reverts.
pub fn revert_reason<T>(result: Result<T>) -> Option<String> {
    match result {
        Err(PrecompileError::Revert(reason)) => Some(reason),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: SlashIndicatorError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_address_integer_hash_and_string_fields() {
        let caller = Address::repeat_byte(0x11);
        let bytes = encoded(SlashIndicatorError::SubmitterNotActiveValidator { caller, status: 3 });
        let decoded = ISlashIndicator::SubmitterNotActiveValidator::abi_decode(&bytes)
            .expect("submitter payload");
        assert_eq!(decoded.caller, caller);
        assert_eq!(decoded.status, 3);

        let bytes = encoded(SlashIndicatorError::EvidenceStale {
            current_block: 30,
            child_block: 10,
            max_age: 15,
        });
        let decoded = ISlashIndicator::EvidenceStale::abi_decode(&bytes).expect("stale payload");
        assert_eq!(decoded.current_block, 30);
        assert_eq!(decoded.child_block, 10);
        assert_eq!(decoded.max_age, 15);

        let hash = B256::repeat_byte(0xab);
        let canonical = B256::repeat_byte(0xcd);
        let bytes = encoded(SlashIndicatorError::EvidenceHashNotCanonical {
            role: "child",
            hash,
            number: 10,
            canonical,
        });
        let decoded =
            ISlashIndicator::EvidenceHashNotCanonical::abi_decode(&bytes).expect("hash payload");
        assert_eq!(decoded.role, "child");
        assert_eq!(decoded.hash, hash);
        assert_eq!(decoded.number, 10);
        assert_eq!(decoded.canonical, canonical);

        let bytes = encoded(SlashIndicatorError::Phase1TxInvalid {
            reason: "decode failed".to_string(),
        });
        let decoded = ISlashIndicator::Phase1TxInvalid::abi_decode(&bytes).expect("reason payload");
        assert_eq!(decoded.reason, "decode failed");
    }
}
