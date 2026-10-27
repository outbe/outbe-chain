// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ISlashIndicator
/// @notice Slash indicator precompile at 0x000000000000000000000000000000000000EE01
interface ISlashIndicator {
    /// Caller-facing domain revert.
    error EvidenceSignerMismatch();
    error ProposalsMustBeSameRound();
    error VotesMustBeSameRound();
    error ProposalsMustDifferForDoubleProposal();
    error ConflictingVotesMustBeDifferentProposals();
    error ConflictingVoteSignaturesInvalid();
    error EvidenceAlreadyProcessed();
    error EvidenceBlockTooShort();
    error EvidencePubkeySlice();
    error EvidenceSignatureSlice();
    error InvalidBlsPublicKey();
    error InvalidBlsSignature();
    error UnexpectedEndOfVarint();
    error VarintOverflow();
    error NonCanonicalVarint();
    error InvalidEvidenceRewardPercent(uint64 percent);
    error EvidenceRewardOverflow();
    error SlashGuardRingSeqOverflow();
    error SubmitterNotActiveValidator(address caller, uint8 status);
    error MissingCommitteeSnapshotForEpoch(uint64 epoch);
    error InvalidCommitteePubkeyInSnapshot();
    error SignerNotRegisteredValidator();
    error EvidenceFieldTruncated();
    error Spe1WrongLength(uint64 expected, uint64 actual);
    error Spe1BadMagic();
    error Spe1BadVersion();
    error Ipe1TooShort(uint64 min_len, uint64 actual);
    error Ipe1BadMagic();
    error Ipe1BadVersion();
    error Ipe1CommitmentLengthExceedsInput();
    error Ipe1TrailingBytes();
    error InvalidVrfEvidenceInputTooShort(uint64 actual, uint64 min_len);
    error InvalidVrfEvidenceBadMagic();
    error InvalidVrfEvidenceUnsupportedVersion(uint8 version, uint8 expected);
    error InvalidVrfEvidenceTrailingBytes(uint64 trailing);
    error InvalidVrfEvidenceFieldExceedsInput(string field, uint64 len, uint64 remaining);
    error EvidenceStale(uint64 current_block, uint64 child_block, uint64 max_age);
    error EvidenceParentChildNumberMismatch(uint64 parent, uint64 child);
    error EvidenceBlockNotInCanonicalWindow(string role, uint64 number);
    error EvidenceHashNotCanonical(string role, bytes32 hash, uint64 number, bytes32 canonical);
    error Phase1TxInvalid(string reason);
    error Phase1CalldataDecodeFailed(string reason);
    error Phase1CalldataNotCertifiedParentAccounting(string kind);
    error MissingCommitteeSnapshotForFinalizedEpoch(uint64 finalized_epoch, bytes32 committee_set_hash);
    error Phase1ProposerNotInCommittee(address proposer, uint64 epoch);
    error IdenticalSeedPartials();
    error InvalidSeedPartialIdentitySignatures();
    error MissingCommitteeSnapshotForRound();
    error EvidenceTooLarge(uint64 actual, uint64 max_bytes);
    error EvidenceEpochStale(uint64 current_epoch, string label, uint64 evidence_epoch, uint64 max_lag);
    error MetadataParentBindingMismatch(
        uint64 ev_parent_num, bytes32 ev_parent_hash, uint64 meta_num, bytes32 meta_hash
    );
    error MetadataEpochMismatch(uint64 finalized_epoch, uint64 child_epoch);
    error ValidVrfProofNotSlashable();
    error NonVrfVerifyRejection(string err);
    error SignerIndexOutOfRange();
    error SignerPubkeyMismatchAtIndex();
    error VrfMaterialVersionMismatch();
    error MissingSnapshotPolynomialCommitment();
    error CommitmentPolynomialHashMismatch();
    error PartialNotIdentitySigned();
    error MalformedCommitmentOrPartial();
    error ValidSeedPartialNotSlashable();
    /// Stored slash-amount or evidence-reward percent is above 100.
    error SlashConfigPercentAbove100(string field, uint64 value);

    /// Emitted when a proposer miss triggers a felony (forced exit + slash).
    event ProposerFelony(address indexed validator, uint64 missCount, uint64 felonyCount);

    /// Emitted when a proposer miss reaches the misdemeanor threshold.
    event ProposerMisdemeanor(address indexed validator, uint64 missCount);

    /// Emitted when a voter miss reaches the misdemeanor threshold.
    event VoterMisdemeanor(address indexed validator, uint64 missCount);

    /// Emitted when a voter miss triggers a felony (forced exit + slash).
    event VoterFelony(address indexed validator, uint64 missCount, uint64 felonyCount);

    /// The precompile emits this event for one evidence felony.
    /// The default submitter reward is 0% of the slashed amount.
    event EvidenceFelonyApplied(
        address indexed validator, address indexed submitter, uint256 slashedAmount, uint256 submitterReward
    );

    /// Emitted when `submitInvalidVrfProofEvidence` accepts a VRF-class
    /// verifier failure and applies the felony to the child block's Phase 1
    /// tx signer. `failureCode` is the canonical class re-derived from
    /// `verify_v2_proof` (NOT the submitter's hint).
    event InvalidVrfProofEvidenceApplied(
        address indexed proposer, address indexed submitter, bytes32 indexed childBlockHash, uint16 failureCode
    );

    /// Emitted when `submitSeedPartialEquivocationEvidence` accepts proof that a
    /// validator identity-signed two different VRF seed partials for the same
    /// `(round, vrf_material_version)` and applies the felony.
    event SeedPartialEquivocationApplied(
        address indexed validator, address indexed submitter, uint64 roundEpoch, uint64 roundView, uint64 vrfVersion
    );

    /// Emitted when `submitInvalidSeedPartialEvidence` accepts proof that a
    /// validator identity-signed a VRF seed partial that fails verification
    /// against the committee polynomial, and applies the felony.
    event InvalidSeedPartialApplied(
        address indexed validator, address indexed submitter, uint64 roundEpoch, uint64 roundView, uint64 vrfVersion
    );

    function submitDoubleProposalEvidence(bytes calldata block1, bytes calldata block2) external;
    function submitConflictingVoteEvidence(bytes calldata vote1, bytes calldata vote2) external;
    /// `ConflictingNotarize`: same signer notarized two different proposals in one view.
    function submitConflictingNotarizeEvidence(bytes calldata block1, bytes calldata block2) external;
    /// `ConflictingFinalize`: same signer finalized two different proposals in one view.
    function submitConflictingFinalizeEvidence(bytes calldata block1, bytes calldata block2) external;
    /// `NullifyFinalize`: same signer both nullified and finalized the same view.
    function submitNullifyFinalizeEvidence(bytes calldata nullifyBlock, bytes calldata finalizeBlock) external;
    /// Submit evidence of an invalid threshold VRF proof in a child
    /// block's Phase 1 system transaction. `evidence` is the wire form of
    /// `InvalidVrfProofEvidence` (see `vrf_evidence.rs`).
    function submitInvalidVrfProofEvidence(bytes calldata evidence) external;
    /// Submit evidence that a validator equivocated on its VRF seed partial:
    /// two different identity-signed partials for the same round/version.
    /// `evidence` is the wire form of `SeedPartialEquivocationEvidence`
    /// (see `seed_partial_evidence.rs`).
    function submitSeedPartialEquivocationEvidence(bytes calldata evidence) external;
    /// Submit evidence that a validator emitted a single INVALID VRF seed
    /// partial (identity-signed, fails verification against the committee
    /// polynomial). `evidence` is the wire form of `InvalidSeedPartialEvidence`.
    function submitInvalidSeedPartialEvidence(bytes calldata evidence) external;
    function getProposerMissCount(address validator) external view returns (uint64);
    function getVoterMissCount(address validator) external view returns (uint64);
    function getFelonyCount(address validator) external view returns (uint64);
}
